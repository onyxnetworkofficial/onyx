//! The credits ledger over HTTP (ADR-0056, contributor loop CL6).
//!
//! One spendable balance per account (an EVM address): its `OnyxPrepaidV2` account. Credit
//! arrives on chain only (`Credited` events: a deposit, an earned share moved in from
//! `OnyxRevenueV2`, a refund, a payout) and is spent off chain with the prepaid vouchers of
//! [`crate::prepaid`]: answers, observation holds and, here, relayed actions.
//!
//! Routes ([`routes`]):
//! - `GET /v1/credits/schedule`: the asset, the relayer and the fixed relay fee of each action.
//! - `GET /v1/credits/{account}`: what the chain shows (`deposited`, `redeemed`, `exitAt`); with
//!   the account's own fresh proof ([`ACCOUNT_PROOF`]) also its arrivals by origin and the
//!   journal's spent, held and spendable.
//! - `GET /v1/credits/{account}/lines`: the account's own journal lines and arrivals (proof
//!   required): which answers it bought is never shown to anyone else (CL6-5).
//! - `POST /v1/relay`: a self-signed action sent by the relayer, paid by a voucher that adds
//!   exactly the action's fee. The voucher is HELD, the call simulated, sent, and the hold charged
//!   only when the transaction is mined without a revert; anything else releases it (CL6-3).
//!
//! The API names no asset: amounts are decimal strings in the asset's smallest unit, beside an
//! `asset` object (`symbol`, `decimals`, `chainId`, `address`), so a token can replace the test
//! asset at mainnet with the same routes, headers and shapes.

use crate::evm_tx::{is_revert, Mined, SendError, Sender};
use crate::prepaid::Voucher;
use crate::prepaid_gate::{self, ChargeError, Held, PrepaidState, RELAY_HOLD_METRIC};
use crate::roots::JsonRpc;
use crate::x402::{self, BoxFuture};
use crate::{rate_scopes, ApiError, ClientKey, Clock};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The request header proving the caller holds the account's key: the base64 JSON
/// `{address, issued, signature}` of [`crate::contrib::ContributorProof`], signing
/// [`account_message`].
pub const ACCOUNT_PROOF: &str = "onyx-account-proof";
/// How old an account proof may be (seconds), as the contributor pass (R-REV-5).
pub const ACCOUNT_PROOF_SECS: u64 = 120;
/// Most lines one `/lines` page returns.
pub const MAX_LINES: usize = 200;
/// Largest `POST /v1/relay` body (a sealed reveal of 1,438 bytes in hex fits with room).
pub const MAX_RELAY_BODY: usize = 8 * 1024;
/// The registry's largest sealed reveal (`OnyxRegistry.MAX_SEALED_BYTES`).
pub const MAX_SEALED_BYTES: usize = 1438;
/// Most used proofs remembered at once; beyond, proofs are refused until old ones expire.
const MAX_USED_PROOFS: usize = 100_000;
/// Most accounts whose recent relays are remembered (oldest dropped first).
const MAX_RATE_ACCOUNTS: usize = 100_000;

/// `keccak256("Credited(address,uint128,uint8)")`, the arrival event of `OnyxPrepaidV2`.
pub fn credited_topic() -> Hash32 {
    keccak256(b"Credited(address,uint128,uint8)")
}

/// The asset the ledger counts in, read from the chain at start (never named in a field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// The token's `symbol()`.
    pub symbol: String,
    /// The token's `decimals()`.
    pub decimals: u8,
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// The token's address.
    pub address: [u8; 20],
}

impl Asset {
    fn json(&self) -> Value {
        json!({
            "symbol": self.symbol,
            "decimals": self.decimals,
            "chainId": self.chain_id,
            "address": format!("0x{}", hex::encode(self.address)),
        })
    }
}

/// An account as `OnyxPrepaidV2.accounts` holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainAccount {
    /// Everything credited, less what was withdrawn.
    pub deposited: u128,
    /// The highest voucher total redeemed.
    pub redeemed: u128,
    /// When a requested exit may be withdrawn (0: none).
    pub exit_at: u64,
}

/// One `Credited` event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    /// 1 deposit, 2 earned, 3 refund, 4 payout.
    pub origin: u8,
    /// The amount, in the asset's smallest unit.
    pub amount: u128,
    /// Its block.
    pub block: u64,
    /// Its transaction.
    pub tx: Hash32,
}

fn origin_name(o: u8) -> &'static str {
    match o {
        1 => "deposit",
        2 => "earned",
        3 => "refund",
        4 => "payout",
        _ => "unknown",
    }
}

/// The chain reads of the credits routes, at the `finalized` block.
pub trait CreditsChain: Send + Sync {
    /// `accounts(who)`.
    fn account<'a>(&'a self, who: [u8; 20]) -> BoxFuture<'a, Result<ChainAccount, String>>;
    /// Every `Credited` event of `who`, oldest first.
    fn arrivals<'a>(&'a self, who: [u8; 20]) -> BoxFuture<'a, Result<Vec<Arrival>, String>>;
}

/// Decode `accounts`' return `(uint128, uint128, uint64)`.
pub fn decode_chain_account(ret: &[u8]) -> Result<ChainAccount, String> {
    if ret.len() != 96 {
        return Err(format!("accounts returned {} bytes, not 96", ret.len()));
    }
    let w = |i: usize| -> Result<u128, String> {
        let b = &ret[i * 32..i * 32 + 32];
        if b[..16].iter().any(|x| *x != 0) {
            return Err("accounts returned a word above uint128".into());
        }
        let mut a = [0u8; 16];
        a.copy_from_slice(&b[16..]);
        Ok(u128::from_be_bytes(a))
    };
    Ok(ChainAccount {
        deposited: w(0)?,
        redeemed: w(1)?,
        exit_at: u64::try_from(w(2)?).map_err(|_| "exitAt above uint64")?,
    })
}

/// [`CreditsChain`] through a node: `eth_call` of `accounts` and `eth_getLogs` of `Credited`,
/// both at `finalized`. Arrivals are indexed once for every account, from the deployment block
/// on, in ranges of `range` blocks (at most `max_ranges` per read, so one read never asks the
/// node for an unbounded scan; a read that is still catching up fails and the next continues).
pub struct ChainCredits<T: JsonRpc> {
    rpc: T,
    contract: [u8; 20],
    range: u64,
    max_ranges: u32,
    index: tokio::sync::Mutex<ArrivalIndex>,
}

struct ArrivalIndex {
    /// The next block to scan.
    next: u64,
    by_account: BTreeMap<[u8; 20], Vec<Arrival>>,
}

impl<T: JsonRpc> ChainCredits<T> {
    /// Reads of the `OnyxPrepaidV2` at `contract`, deployed at block `from_block`.
    pub fn new(rpc: T, contract: [u8; 20], from_block: u64) -> Self {
        ChainCredits {
            rpc,
            contract,
            range: 10_000,
            max_ranges: 50,
            index: tokio::sync::Mutex::new(ArrivalIndex {
                next: from_block,
                by_account: BTreeMap::new(),
            }),
        }
    }

    /// Scan `range` blocks per `eth_getLogs`, at most `max_ranges` per read.
    pub fn with_range(mut self, range: u64, max_ranges: u32) -> Self {
        self.range = range.max(1);
        self.max_ranges = max_ranges.max(1);
        self
    }

    async fn finalized(&self) -> Result<u64, String> {
        let b = self
            .rpc
            .call("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        let n = b["number"]
            .as_str()
            .ok_or("finalized block has no number")?;
        u64::from_str_radix(n.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
    }

    fn decode_log(&self, l: &Value) -> Result<([u8; 20], Arrival), String> {
        let addr = l["address"].as_str().unwrap_or_default().to_lowercase();
        if addr != format!("0x{}", hex::encode(self.contract)) {
            return Err(format!("a log from {addr}"));
        }
        let topics = l["topics"].as_array().ok_or("log without topics")?;
        let t = |i: usize| -> Result<[u8; 32], String> {
            let s = topics
                .get(i)
                .and_then(Value::as_str)
                .ok_or("missing topic")?;
            hex::decode(s.trim_start_matches("0x"))
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| "topic is not 32 bytes".to_string())
        };
        if topics.len() != 2 || t(0)? != credited_topic().0 {
            return Err("not a Credited log".into());
        }
        let acct = t(1)?;
        if acct[..12].iter().any(|b| *b != 0) {
            return Err("account topic is not an address".into());
        }
        let data = hex::decode(
            l["data"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("0x"),
        )
        .map_err(|e| e.to_string())?;
        if data.len() != 64
            || data[..16].iter().any(|b| *b != 0)
            || data[32..63].iter().any(|b| *b != 0)
        {
            return Err("Credited data is not (uint128, uint8)".into());
        }
        let mut amt = [0u8; 16];
        amt.copy_from_slice(&data[16..32]);
        let q = |k: &str| -> Result<u64, String> {
            u64::from_str_radix(
                l[k].as_str()
                    .ok_or("log without block")?
                    .trim_start_matches("0x"),
                16,
            )
            .map_err(|e| e.to_string())
        };
        let tx: [u8; 32] = hex::decode(
            l["transactionHash"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("0x"),
        )
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "transaction hash is not 32 bytes")?;
        let mut who = [0u8; 20];
        who.copy_from_slice(&acct[12..]);
        Ok((
            who,
            Arrival {
                origin: data[63],
                amount: u128::from_be_bytes(amt),
                block: q("blockNumber")?,
                tx: Hash32(tx),
            },
        ))
    }
}

impl<T: JsonRpc> CreditsChain for ChainCredits<T> {
    fn account<'a>(&'a self, who: [u8; 20]) -> BoxFuture<'a, Result<ChainAccount, String>> {
        Box::pin(async move {
            let r = self
                .rpc
                .call(
                    "eth_call",
                    json!([{
                        "to": format!("0x{}", hex::encode(self.contract)),
                        "data": format!("0x{}", hex::encode(crate::prepaid_chain::accounts_calldata(&who))),
                    }, "finalized"]),
                )
                .await?;
            let ret = hex::decode(r.as_str().unwrap_or_default().trim_start_matches("0x"))
                .map_err(|e| format!("accounts: {e}"))?;
            decode_chain_account(&ret)
        })
    }

    fn arrivals<'a>(&'a self, who: [u8; 20]) -> BoxFuture<'a, Result<Vec<Arrival>, String>> {
        Box::pin(async move {
            let mut ix = self.index.lock().await;
            let head = self.finalized().await?;
            let mut ranges = 0;
            while ix.next <= head {
                if ranges == self.max_ranges {
                    return Err(format!(
                        "the arrival index is catching up (block {} of {head})",
                        ix.next
                    ));
                }
                let to = ix.next.saturating_add(self.range - 1).min(head);
                let logs = self
                    .rpc
                    .call(
                        "eth_getLogs",
                        json!([{
                            "address": format!("0x{}", hex::encode(self.contract)),
                            "topics": [credited_topic().to_string()],
                            "fromBlock": format!("0x{:x}", ix.next),
                            "toBlock": format!("0x{to:x}"),
                        }]),
                    )
                    .await?;
                let mut found = Vec::new();
                for l in logs.as_array().ok_or("eth_getLogs did not answer a list")? {
                    if l["removed"].as_bool() == Some(true) {
                        continue;
                    }
                    found.push(self.decode_log(l)?);
                }
                found.sort_by_key(|(_, a)| a.block);
                for (w, a) in found {
                    ix.by_account.entry(w).or_default().push(a);
                }
                ix.next = to + 1;
                ranges += 1;
            }
            Ok(ix.by_account.get(&who).cloned().unwrap_or_default())
        })
    }
}

/// The text an account signs to read its own ledger: one account, one time, one ledger.
pub fn account_message(account: &[u8; 20], issued: u64, ledger: &str) -> String {
    format!(
        "onyx-account-read-v1\naccount: 0x{}\nissued: {issued}\nledger: {ledger}",
        hex::encode(account)
    )
}

fn eip191(msg: &[u8]) -> [u8; 32] {
    let mut b = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    b.extend_from_slice(msg);
    keccak256(&b).0
}

/// A header value proving `secret`'s address for its own ledger at `issued` (clients, tests).
pub fn sign_account_proof(secret: &[u8; 32], issued: u64, ledger: &str) -> Option<String> {
    let address = x402::exact_evm::address(secret)?;
    let sk = secp256k1::SecretKey::from_slice(secret).ok()?;
    let digest = eip191(account_message(&address, issued, ledger).as_bytes());
    let (rid, rs) = secp256k1::Secp256k1::signing_only()
        .sign_ecdsa_recoverable(&secp256k1::Message::from_digest(digest), &sk)
        .serialize_compact();
    let mut sig = rs.to_vec();
    sig.push(27 + u8::try_from(rid.to_i32()).ok()?);
    Some(x402::encode_header(&crate::contrib::ContributorProof {
        address: format!("0x{}", hex::encode(address)),
        issued,
        signature: format!("0x{}", hex::encode(sig)),
    }))
}

/// Check an [`ACCOUNT_PROOF`] for `account` on `ledger` at `now`: well formed, issued within
/// [`ACCOUNT_PROOF_SECS`] (and at most 30 s ahead), the address is `account`, a canonical
/// signature (v 27 or 28, low s) by it. Returns the proof's replay key.
pub fn verify_account_proof(
    header: &str,
    account: &[u8; 20],
    ledger: &str,
    now: u64,
) -> Result<Hash32, &'static str> {
    let p: crate::contrib::ContributorProof =
        x402::decode_header(header).map_err(|_| "malformed proof")?;
    let address: [u8; 20] = hex::decode(p.address.strip_prefix("0x").ok_or("bad address")?)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or("bad address")?;
    if address != *account {
        return Err("the proof is for another account");
    }
    let sig = hex::decode(p.signature.strip_prefix("0x").ok_or("bad signature")?)
        .map_err(|_| "bad signature")?;
    if p.issued > now.saturating_add(30) || now.saturating_sub(p.issued) > ACCOUNT_PROOF_SECS {
        return Err("proof expired or from the future");
    }
    if sig.len() != 65 || !matches!(sig[64], 27 | 28) {
        return Err("signature is not canonical");
    }
    let digest = eip191(account_message(account, p.issued, ledger).as_bytes());
    // `recover` refuses a high s.
    match x402::exact_evm::recover(&digest, &sig) {
        Some(a) if a == address => {
            let mut k = address.to_vec();
            k.extend_from_slice(&digest);
            Ok(keccak256(&k))
        }
        _ => Err("signature does not match the account"),
    }
}

/// An action the relayer sends (ADR-0056 item 4): only calls that carry the actor's own
/// signature, so the relayer can never act for anyone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelayKind {
    /// `OnyxRegistry.postCommit`, signed by the observer.
    PostCommit,
    /// `OnyxRegistry.postReceipt`, signed by the draw's receipt signer for the observer.
    PostReceipt,
    /// `OnyxRegistry.postSealedReveal`, signed by the observer.
    PostSealedReveal,
    /// `OnyxPrepaidV2.depositWithAuthorization`, an EIP-3009 authorization signed by the
    /// account.
    Deposit,
}

impl RelayKind {
    /// Every kind.
    pub const ALL: [RelayKind; 4] = [
        RelayKind::PostCommit,
        RelayKind::PostReceipt,
        RelayKind::PostSealedReveal,
        RelayKind::Deposit,
    ];

    /// The API's name.
    pub fn name(self) -> &'static str {
        match self {
            RelayKind::PostCommit => "post_commit",
            RelayKind::PostReceipt => "post_receipt",
            RelayKind::PostSealedReveal => "post_sealed_reveal",
            RelayKind::Deposit => "deposit",
        }
    }

    /// The kind named `s`.
    pub fn parse(s: &str) -> Option<RelayKind> {
        RelayKind::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// The fixed, published relay fee of each action, in the asset's smallest unit (ADR-0056 item
/// 4: no ETH-to-credit price feed). A kind not listed is not relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeSchedule {
    fees: Vec<(RelayKind, u128)>,
}

/// Why a fee schedule is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    /// Not `name=fee` pairs separated by commas.
    #[error("relay fees: {0:?} is not name=fee")]
    Malformed(String),
    /// An action that is not relayed.
    #[error("relay fees: unknown action {0:?}")]
    Unknown(String),
    /// The same action twice.
    #[error("relay fees: {0} is listed twice")]
    Duplicate(&'static str),
    /// A fee of zero (a relay is never free: R-CR-7) or above the cap.
    #[error("relay fees: {action} must be 1..={max}")]
    OutOfRange {
        /// The action.
        action: &'static str,
        /// The highest fee taken.
        max: u128,
    },
    /// No action at all.
    #[error("relay fees: no action")]
    Empty,
}

impl FeeSchedule {
    /// A schedule of `fees`, each in `1..=max_fee`, each kind at most once.
    pub fn new(fees: &[(RelayKind, u128)], max_fee: u128) -> Result<FeeSchedule, ScheduleError> {
        if fees.is_empty() {
            return Err(ScheduleError::Empty);
        }
        let mut v = fees.to_vec();
        v.sort();
        for w in v.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(ScheduleError::Duplicate(w[0].0.name()));
            }
        }
        for (k, f) in &v {
            if *f == 0 || *f > max_fee {
                return Err(ScheduleError::OutOfRange {
                    action: k.name(),
                    max: max_fee,
                });
            }
        }
        Ok(FeeSchedule { fees: v })
    }

    /// Parse `post_commit=1000,post_receipt=1000,...`.
    pub fn parse(spec: &str, max_fee: u128) -> Result<FeeSchedule, ScheduleError> {
        let mut fees = Vec::new();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, f) = part
                .split_once('=')
                .ok_or_else(|| ScheduleError::Malformed(part.into()))?;
            let kind =
                RelayKind::parse(k.trim()).ok_or_else(|| ScheduleError::Unknown(k.into()))?;
            let f = f.trim();
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ScheduleError::Malformed(part.into()));
            }
            let fee = f
                .parse::<u128>()
                .map_err(|_| ScheduleError::Malformed(part.into()))?;
            fees.push((kind, fee));
        }
        FeeSchedule::new(&fees, max_fee)
    }

    /// The fee of `kind`, if it is relayed.
    pub fn fee(&self, kind: RelayKind) -> Option<u128> {
        self.fees.iter().find(|(k, _)| *k == kind).map(|(_, f)| *f)
    }

    /// Keccak-256 of the schedule's canonical text, so a client can pin what it was quoted.
    pub fn hash(&self) -> Hash32 {
        let text: Vec<String> = self
            .fees
            .iter()
            .map(|(k, f)| format!("{}={f}", k.name()))
            .collect();
        keccak256(format!("onyx-relay-fees-v1\n{}", text.join(",")).as_bytes())
    }

    fn json(&self) -> Value {
        Value::Array(
            self.fees
                .iter()
                .map(|(k, f)| json!({ "action": k.name(), "fee": f.to_string() }))
                .collect(),
        )
    }
}

/// One relayed action, parsed and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayAction {
    /// `postCommit(fact, attempt, observer, round, commit, sig)`.
    PostCommit {
        /// The fact key.
        fact: Hash32,
        /// The draw attempt.
        attempt: u32,
        /// The observer (the actor).
        observer: [u8; 20],
        /// The round id.
        round: Hash32,
        /// The salted commit.
        commit: Hash32,
        /// The observer's EIP-191 signature of the commit message.
        sig: [u8; 65],
    },
    /// `postReceipt(fact, attempt, observer, round, commit, sig)`.
    PostReceipt {
        /// The fact key.
        fact: Hash32,
        /// The draw attempt.
        attempt: u32,
        /// The observer (the actor).
        observer: [u8; 20],
        /// The round id.
        round: Hash32,
        /// The commit the receipt names.
        commit: Hash32,
        /// The receipt signer's EIP-191 signature.
        sig: [u8; 65],
    },
    /// `postSealedReveal(fact, attempt, observer, round, ex, ey, ct, keyProof, sig)`.
    PostSealedReveal {
        /// The fact key.
        fact: Hash32,
        /// The draw attempt.
        attempt: u32,
        /// The observer (the actor).
        observer: [u8; 20],
        /// The round id.
        round: Hash32,
        /// The ephemeral key, x.
        ex: Hash32,
        /// The ephemeral key, y.
        ey: Hash32,
        /// The ciphertext.
        ct: Vec<u8>,
        /// The proof of the ephemeral secret (52 bytes).
        key_proof: Vec<u8>,
        /// The observer's EIP-191 signature.
        sig: [u8; 65],
    },
    /// `depositWithAuthorization(from, amount, validAfter, validBefore, nonce, sig)`.
    Deposit {
        /// The account (the actor).
        from: [u8; 20],
        /// The amount paid in.
        amount: u128,
        /// EIP-3009 `validAfter`.
        valid_after: u64,
        /// EIP-3009 `validBefore`.
        valid_before: u64,
        /// EIP-3009 nonce.
        nonce: Hash32,
        /// The account's EIP-712 `ReceiveWithAuthorization` signature.
        sig: [u8; 65],
    },
}

/// Why a relay request is refused before anything is held or sent.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    /// The body or an argument is not exactly the expected shape.
    #[error("invalid relay request: {0}")]
    Invalid(String),
    /// The actor's signature (or the receipt signer's) is missing, malformed or not theirs.
    #[error("the action does not carry its actor's signature: {0}")]
    NotSigned(&'static str),
}

fn arg<'a>(args: &'a serde_json::Map<String, Value>, k: &str) -> Result<&'a str, RelayError> {
    args.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| RelayError::Invalid(format!("{k} must be a string")))
}

fn hex_arg<const N: usize>(
    args: &serde_json::Map<String, Value>,
    k: &str,
) -> Result<[u8; N], RelayError> {
    let b = hex_bytes(args, k, N)?;
    b.try_into()
        .map_err(|_| RelayError::Invalid(format!("{k} must be {N} bytes")))
}

fn hex_bytes(
    args: &serde_json::Map<String, Value>,
    k: &str,
    max: usize,
) -> Result<Vec<u8>, RelayError> {
    let s = arg(args, k)?;
    let h = s
        .strip_prefix("0x")
        .ok_or_else(|| RelayError::Invalid(format!("{k} must be 0x hex")))?;
    if h.len() > 2 * max || h.bytes().any(|c| c.is_ascii_uppercase()) {
        return Err(RelayError::Invalid(format!(
            "{k} must be lowercase 0x hex of at most {max} bytes"
        )));
    }
    hex::decode(h).map_err(|_| RelayError::Invalid(format!("{k} must be 0x hex")))
}

fn dec_arg(args: &serde_json::Map<String, Value>, k: &str) -> Result<u128, RelayError> {
    let s = arg(args, k)?;
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(RelayError::Invalid(format!("{k} must be a decimal string")));
    }
    s.parse()
        .map_err(|_| RelayError::Invalid(format!("{k} is out of range")))
}

fn u32_arg(args: &serde_json::Map<String, Value>, k: &str) -> Result<u32, RelayError> {
    args.get(k)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| RelayError::Invalid(format!("{k} must be a u32")))
}

fn nonzero(h: Hash32, k: &str) -> Result<Hash32, RelayError> {
    if h == Hash32([0; 32]) {
        return Err(RelayError::Invalid(format!("{k} must not be zero")));
    }
    Ok(h)
}

/// `keccak256("ReceiveWithAuthorization(address from,address to,uint256 value,uint256
/// validAfter,uint256 validBefore,bytes32 nonce)")`, EIP-3009.
pub fn receive_typehash() -> Hash32 {
    keccak256(
        b"ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)",
    )
}

/// The EIP-712 digest of an EIP-3009 `ReceiveWithAuthorization` in the token's domain.
pub fn receive_digest(
    token_domain: &[u8; 32],
    from: &[u8; 20],
    to: &[u8; 20],
    value: u128,
    valid_after: u64,
    valid_before: u64,
    nonce: &Hash32,
) -> [u8; 32] {
    let mut s = receive_typehash().0.to_vec();
    s.extend_from_slice(&word_addr(from));
    s.extend_from_slice(&word_addr(to));
    s.extend_from_slice(&word(value));
    s.extend_from_slice(&word(u128::from(valid_after)));
    s.extend_from_slice(&word(u128::from(valid_before)));
    s.extend_from_slice(&nonce.0);
    let mut m = b"\x19\x01".to_vec();
    m.extend_from_slice(token_domain);
    m.extend_from_slice(&keccak256(&s).0);
    keccak256(&m).0
}

fn word(n: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&n.to_be_bytes());
    w
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

enum Abi<'a> {
    W([u8; 32]),
    B(&'a [u8]),
}

fn abi_call(sig: &str, args: &[Abi]) -> Vec<u8> {
    let mut out = keccak256(sig.as_bytes()).0[..4].to_vec();
    let mut tail: Vec<u8> = Vec::new();
    let heads = 32 * args.len();
    for a in args {
        match a {
            Abi::W(w) => out.extend_from_slice(w),
            Abi::B(b) => {
                out.extend_from_slice(&word((heads + tail.len()) as u128));
                tail.extend_from_slice(&word(b.len() as u128));
                tail.extend_from_slice(b);
                tail.resize(tail.len().div_ceil(32) * 32, 0);
            }
        }
    }
    out.extend_from_slice(&tail);
    out
}

impl RelayAction {
    /// Parse `{action, args}`; every argument exactly once, no other key.
    pub fn parse(body: &Value) -> Result<RelayAction, RelayError> {
        let o = body
            .as_object()
            .ok_or_else(|| RelayError::Invalid("the body must be an object".into()))?;
        if o.len() != 2 {
            return Err(RelayError::Invalid(
                "the body is exactly {action, args}".into(),
            ));
        }
        let name = o
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| RelayError::Invalid("action must be a string".into()))?;
        let kind = RelayKind::parse(name)
            .ok_or_else(|| RelayError::Invalid(format!("unknown action {name:?}")))?;
        let a = o
            .get("args")
            .and_then(Value::as_object)
            .ok_or_else(|| RelayError::Invalid("args must be an object".into()))?;
        let want: &[&str] = match kind {
            RelayKind::PostCommit | RelayKind::PostReceipt => &[
                "fact",
                "attempt",
                "observer",
                "round",
                "commit",
                "signature",
            ],
            RelayKind::PostSealedReveal => &[
                "fact",
                "attempt",
                "observer",
                "round",
                "ex",
                "ey",
                "ciphertext",
                "keyProof",
                "signature",
            ],
            RelayKind::Deposit => &[
                "from",
                "amount",
                "validAfter",
                "validBefore",
                "nonce",
                "signature",
            ],
        };
        if a.len() != want.len() || want.iter().any(|k| !a.contains_key(*k)) {
            return Err(RelayError::Invalid(format!(
                "{} takes exactly {}",
                kind.name(),
                want.join(", ")
            )));
        }
        let h = |k: &str| hex_arg::<32>(a, k).map(Hash32);
        Ok(match kind {
            RelayKind::PostCommit | RelayKind::PostReceipt => {
                let (fact, attempt, observer, round, commit, sig) = (
                    h("fact")?,
                    u32_arg(a, "attempt")?,
                    hex_arg::<20>(a, "observer")?,
                    nonzero(h("round")?, "round")?,
                    nonzero(h("commit")?, "commit")?,
                    hex_arg::<65>(a, "signature")?,
                );
                if kind == RelayKind::PostCommit {
                    RelayAction::PostCommit {
                        fact,
                        attempt,
                        observer,
                        round,
                        commit,
                        sig,
                    }
                } else {
                    RelayAction::PostReceipt {
                        fact,
                        attempt,
                        observer,
                        round,
                        commit,
                        sig,
                    }
                }
            }
            RelayKind::PostSealedReveal => {
                let ct = hex_bytes(a, "ciphertext", MAX_SEALED_BYTES)?;
                if ct.is_empty() {
                    return Err(RelayError::Invalid("ciphertext must not be empty".into()));
                }
                let key_proof = hex_arg::<52>(a, "keyProof")?.to_vec();
                RelayAction::PostSealedReveal {
                    fact: h("fact")?,
                    attempt: u32_arg(a, "attempt")?,
                    observer: hex_arg::<20>(a, "observer")?,
                    round: nonzero(h("round")?, "round")?,
                    ex: h("ex")?,
                    ey: h("ey")?,
                    ct,
                    key_proof,
                    sig: hex_arg::<65>(a, "signature")?,
                }
            }
            RelayKind::Deposit => {
                let amount = dec_arg(a, "amount")?;
                if amount == 0 {
                    return Err(RelayError::Invalid("amount must not be zero".into()));
                }
                let t = |k: &str| {
                    dec_arg(a, k).and_then(|n| {
                        u64::try_from(n)
                            .map_err(|_| RelayError::Invalid(format!("{k} is out of range")))
                    })
                };
                RelayAction::Deposit {
                    from: hex_arg::<20>(a, "from")?,
                    amount,
                    valid_after: t("validAfter")?,
                    valid_before: t("validBefore")?,
                    nonce: h("nonce")?,
                    sig: hex_arg::<65>(a, "signature")?,
                }
            }
        })
    }

    /// The kind.
    pub fn kind(&self) -> RelayKind {
        match self {
            RelayAction::PostCommit { .. } => RelayKind::PostCommit,
            RelayAction::PostReceipt { .. } => RelayKind::PostReceipt,
            RelayAction::PostSealedReveal { .. } => RelayKind::PostSealedReveal,
            RelayAction::Deposit { .. } => RelayKind::Deposit,
        }
    }

    /// The account the action is for, which must also pay its fee.
    pub fn actor(&self) -> [u8; 20] {
        match self {
            RelayAction::PostCommit { observer, .. }
            | RelayAction::PostReceipt { observer, .. }
            | RelayAction::PostSealedReveal { observer, .. } => *observer,
            RelayAction::Deposit { from, .. } => *from,
        }
    }

    /// What the action pays into the actor's balance (a deposit), for its own fee's hold.
    pub fn incoming(&self) -> u128 {
        match self {
            RelayAction::Deposit { amount, .. } => *amount,
            _ => 0,
        }
    }

    /// The signature check, before anything is held or sent (CL6-2): the observer's own
    /// signature of the commit or sealed reveal; a canonical receipt signature (the chain then
    /// checks it is the draw's receipt signer's); the account's own EIP-3009 signature to
    /// `prepaid` in the token's domain.
    pub fn check_signature(
        &self,
        token_domain: Option<&[u8; 32]>,
        prepaid: &[u8; 20],
    ) -> Result<(), RelayError> {
        let eip191 =
            |msg: &str, sig: &[u8; 65]| onyx_labels::attest::eip191_recover(msg.as_bytes(), sig);
        match self {
            RelayAction::PostCommit {
                attempt,
                observer,
                round,
                commit,
                sig,
                ..
            } => {
                let m = onyx_coordinator::observe::observation_commit_message(
                    round, *attempt, observer, commit,
                );
                if eip191(&m, sig) != Some(*observer) {
                    return Err(RelayError::NotSigned(
                        "the commit is not signed by the observer",
                    ));
                }
            }
            RelayAction::PostReceipt {
                attempt,
                observer,
                round,
                commit,
                sig,
                ..
            } => {
                let m = onyx_coordinator::observe::reveal_receipt_message(
                    round, *attempt, observer, commit,
                );
                match eip191(&m, sig) {
                    Some(s) if s != *observer => {}
                    _ => {
                        return Err(RelayError::NotSigned(
                            "the receipt is not signed by a receipt signer",
                        ))
                    }
                }
            }
            RelayAction::PostSealedReveal {
                attempt,
                observer,
                round,
                ex,
                ey,
                ct,
                sig,
                ..
            } => {
                let mut point = [0u8; 64];
                point[..32].copy_from_slice(&ex.0);
                point[32..].copy_from_slice(&ey.0);
                let m = onyx_coordinator::sealed::sealed_reveal_message(
                    round,
                    *attempt,
                    observer,
                    &point,
                    &keccak256(ct),
                );
                if eip191(&m, sig) != Some(*observer) {
                    return Err(RelayError::NotSigned(
                        "the sealed reveal is not signed by the observer",
                    ));
                }
            }
            RelayAction::Deposit {
                from,
                amount,
                valid_after,
                valid_before,
                nonce,
                sig,
            } => {
                let Some(domain) = token_domain else {
                    return Err(RelayError::NotSigned(
                        "the token's signing domain is unknown",
                    ));
                };
                let d = receive_digest(
                    domain,
                    from,
                    prepaid,
                    *amount,
                    *valid_after,
                    *valid_before,
                    nonce,
                );
                if !matches!(sig[64], 27 | 28) || x402::exact_evm::recover(&d, sig) != Some(*from) {
                    return Err(RelayError::NotSigned(
                        "the deposit is not signed by its account",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The call's calldata.
    pub fn calldata(&self) -> Vec<u8> {
        let w32 = |h: &Hash32| Abi::W(h.0);
        match self {
            RelayAction::PostCommit {
                fact,
                attempt,
                observer,
                round,
                commit,
                sig,
            }
            | RelayAction::PostReceipt {
                fact,
                attempt,
                observer,
                round,
                commit,
                sig,
            } => {
                let f = if self.kind() == RelayKind::PostCommit {
                    "postCommit(bytes32,uint32,address,bytes32,bytes32,bytes)"
                } else {
                    "postReceipt(bytes32,uint32,address,bytes32,bytes32,bytes)"
                };
                abi_call(
                    f,
                    &[
                        w32(fact),
                        Abi::W(word(u128::from(*attempt))),
                        Abi::W(word_addr(observer)),
                        w32(round),
                        w32(commit),
                        Abi::B(sig),
                    ],
                )
            }
            RelayAction::PostSealedReveal {
                fact,
                attempt,
                observer,
                round,
                ex,
                ey,
                ct,
                key_proof,
                sig,
            } => abi_call(
                "postSealedReveal(bytes32,uint32,address,bytes32,bytes32,bytes32,bytes,bytes,bytes)",
                &[
                    w32(fact),
                    Abi::W(word(u128::from(*attempt))),
                    Abi::W(word_addr(observer)),
                    w32(round),
                    w32(ex),
                    w32(ey),
                    Abi::B(ct),
                    Abi::B(key_proof),
                    Abi::B(sig),
                ],
            ),
            RelayAction::Deposit {
                from,
                amount,
                valid_after,
                valid_before,
                nonce,
                sig,
            } => abi_call(
                "depositWithAuthorization(address,uint128,uint256,uint256,bytes32,bytes)",
                &[
                    Abi::W(word_addr(from)),
                    Abi::W(word(*amount)),
                    Abi::W(word(u128::from(*valid_after))),
                    Abi::W(word(u128::from(*valid_before))),
                    w32(nonce),
                    Abi::B(sig),
                ],
            ),
        }
    }
}

/// Why a relay simulation failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SimError {
    /// The call reverts: it would fail on chain.
    Revert(String),
    /// The node could not tell.
    Rpc(String),
}

/// What the relay needs from the chain and the relayer's key.
pub trait RelaySend: Send + Sync {
    /// The relayer's address.
    fn address(&self) -> [u8; 20];
    /// The latest block's base fee (wei).
    fn base_fee<'a>(&'a self) -> BoxFuture<'a, Result<u128, String>>;
    /// Simulate the call from the relayer at `latest`; its gas estimate.
    fn simulate<'a>(&'a self, to: [u8; 20], data: Vec<u8>) -> BoxFuture<'a, Result<u64, SimError>>;
    /// Send the call and wait for its receipt at most `deadline`.
    fn send<'a>(
        &'a self,
        to: [u8; 20],
        data: Vec<u8>,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<Mined, SendError>>;
}

impl<T: JsonRpc + 'static> RelaySend for Sender<T> {
    fn address(&self) -> [u8; 20] {
        Sender::address(self)
    }

    fn base_fee<'a>(&'a self) -> BoxFuture<'a, Result<u128, String>> {
        Box::pin(async move {
            let b = self
                .rpc()
                .call("eth_getBlockByNumber", json!(["latest", false]))
                .await?;
            let s = b["baseFeePerGas"]
                .as_str()
                .ok_or("block without a base fee")?;
            u128::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
        })
    }

    fn simulate<'a>(&'a self, to: [u8; 20], data: Vec<u8>) -> BoxFuture<'a, Result<u64, SimError>> {
        Box::pin(async move {
            let r = self
                .rpc()
                .call(
                    "eth_estimateGas",
                    json!([{
                        "from": format!("0x{}", hex::encode(Sender::address(self))),
                        "to": format!("0x{}", hex::encode(to)),
                        "data": format!("0x{}", hex::encode(&data)),
                    }]),
                )
                .await
                .map_err(|e| {
                    if is_revert(&e) {
                        SimError::Revert(e)
                    } else {
                        SimError::Rpc(e)
                    }
                })?;
            let s = r
                .as_str()
                .ok_or_else(|| SimError::Rpc(format!("not a quantity: {r}")))?;
            u64::from_str_radix(s.trim_start_matches("0x"), 16)
                .map_err(|e| SimError::Rpc(e.to_string()))
        })
    }

    fn send<'a>(
        &'a self,
        to: [u8; 20],
        data: Vec<u8>,
        deadline: Duration,
    ) -> BoxFuture<'a, Result<Mined, SendError>> {
        Box::pin(async move { Sender::send(self, &to, &data, 0, deadline).await })
    }
}

/// The relay's limits (ADR-0056 item 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayPolicy {
    /// Relays one account may ask for in any hour (counted before anything is held or sent).
    pub per_account_per_hour: u32,
    /// Relays of one account that may fail (simulation or chain revert) in one UTC day; after
    /// that the account is refused until the next day, so it cannot make the relayer pay gas
    /// for reverts it never pays for.
    pub reverts_per_day: u32,
    /// Gas the relayer may spend in one UTC day, reserved at the simulated estimate.
    pub daily_gas_budget: u64,
    /// Gas one account's relays may use in one UTC day (review CL6-R1-2: a flat fee must not let
    /// one account spend the whole daily budget).
    pub account_gas_per_day: u64,
    /// The highest base fee (wei) the relayer sends at; above it, `503 relay_busy`.
    pub max_base_fee: u128,
    /// How long one relay may wait for its receipt.
    pub send_timeout: Duration,
    /// How long a relay hold may last (well under the prepaid hold limit).
    pub hold_secs: u64,
}

impl Default for RelayPolicy {
    fn default() -> Self {
        RelayPolicy {
            per_account_per_hour: 60,
            reverts_per_day: 3,
            daily_gas_budget: 20_000_000,
            // One sealed reveal of the largest size (about 1M gas, review CL6-R1-2) fits twice.
            account_gas_per_day: 2_500_000,
            // 0.1 gwei: two orders of magnitude above Base Sepolia's usual base fee.
            max_base_fee: 100_000_000,
            send_timeout: Duration::from_secs(10),
            hold_secs: 120,
        }
    }
}

#[derive(Default)]
struct RelayInner {
    /// Relay times of each account in the last hour.
    recent: BTreeMap<[u8; 20], VecDeque<u64>>,
    /// Failed relays of each account: (UTC day, count).
    failed: BTreeMap<[u8; 20], (u64, u32)>,
    /// The UTC day of `gas_today`.
    day: u64,
    /// Gas reserved or spent today.
    gas_today: u64,
    /// Gas reserved or spent today by each account: (UTC day, gas).
    account_gas: BTreeMap<[u8; 20], (u64, u64)>,
}

/// The relayer: its key, fees, targets and limits.
pub struct Relay {
    sender: Arc<dyn RelaySend>,
    schedule: FeeSchedule,
    policy: RelayPolicy,
    /// `OnyxRegistry`, for the registry posts (none: they are not relayed).
    registry: Option<[u8; 20]>,
    /// The token's EIP-712 domain separator, for the deposit check (none: no deposits).
    token_domain: Option<[u8; 32]>,
    inner: Mutex<RelayInner>,
}

impl Relay {
    /// A relayer sending with `sender` at the fees of `schedule`.
    pub fn new(
        sender: Arc<dyn RelaySend>,
        schedule: FeeSchedule,
        policy: RelayPolicy,
        registry: Option<[u8; 20]>,
        token_domain: Option<[u8; 32]>,
    ) -> Relay {
        Relay {
            sender,
            schedule,
            policy,
            registry,
            token_domain,
            inner: Mutex::new(RelayInner::default()),
        }
    }

    /// The fee of `kind`, when it is relayed here (its target configured).
    pub fn fee(&self, kind: RelayKind) -> Option<u128> {
        let reachable = match kind {
            RelayKind::Deposit => self.token_domain.is_some(),
            _ => self.registry.is_some(),
        };
        self.schedule.fee(kind).filter(|_| reachable)
    }

    /// Count one relay of `who` at `now` (CL6-4): refused over the hourly cap, or after too
    /// many failed relays today. Called only once the account's own voucher is held (review
    /// CL6-R1-4: nobody else can spend an account's relays).
    fn admit(&self, who: [u8; 20], now: u64) -> Result<(), &'static str> {
        let mut i = self.inner.lock().map_err(|_| "relay state poisoned")?;
        let day = now / 86_400;
        if i.failed
            .get(&who)
            .is_some_and(|(d, n)| *d == day && *n >= self.policy.reverts_per_day)
        {
            return Err("too many failed relays from this account today");
        }
        if i.recent.len() >= MAX_RATE_ACCOUNTS && !i.recent.contains_key(&who) {
            i.recent
                .retain(|_, q| q.back().is_some_and(|t| now.saturating_sub(*t) < 3600));
            if i.recent.len() >= MAX_RATE_ACCOUNTS {
                return Err("the relay is busy");
            }
        }
        let q = i.recent.entry(who).or_default();
        while q.front().is_some_and(|t| now.saturating_sub(*t) >= 3600) {
            q.pop_front();
        }
        if q.len() >= self.policy.per_account_per_hour as usize {
            return Err("this account's relays per hour are spent");
        }
        q.push_back(now);
        Ok(())
    }

    fn failed(&self, who: [u8; 20], now: u64) {
        if let Ok(mut i) = self.inner.lock() {
            let day = now / 86_400;
            let e = i.failed.entry(who).or_insert((day, 0));
            if e.0 != day {
                *e = (day, 0);
            }
            e.1 = e.1.saturating_add(1);
        }
    }

    /// Reserve `gas` from today's budget and from `who`'s share of it.
    fn reserve(&self, who: [u8; 20], gas: u64, now: u64) -> Result<(), &'static str> {
        let mut i = self.inner.lock().map_err(|_| "relay state poisoned")?;
        let day = now / 86_400;
        if i.day != day {
            i.day = day;
            i.gas_today = 0;
            i.account_gas.clear();
        }
        let mine = i.account_gas.get(&who).map_or(0, |(_, g)| *g);
        let total = i
            .gas_today
            .checked_add(gas)
            .filter(|g| *g <= self.policy.daily_gas_budget);
        let own = mine
            .checked_add(gas)
            .filter(|g| *g <= self.policy.account_gas_per_day);
        match (total, own) {
            (Some(t), Some(o)) => {
                i.gas_today = t;
                i.account_gas.insert(who, (day, o));
                Ok(())
            }
            (None, _) => Err("the relay's gas budget for today is spent"),
            (_, None) => Err("this account's share of the relay's gas for today is spent"),
        }
    }

    /// Replace a reservation by the gas used (none: nothing was sent).
    fn settle(&self, who: [u8; 20], reserved: u64, used: Option<u64>, now: u64) {
        if let Ok(mut i) = self.inner.lock() {
            if i.day == now / 86_400 {
                let fix = |g: u64| g.saturating_sub(reserved).saturating_add(used.unwrap_or(0));
                i.gas_today = fix(i.gas_today);
                if let Some(e) = i.account_gas.get_mut(&who) {
                    e.1 = fix(e.1);
                }
            }
        }
    }
}

/// How a relay ended after its hold was taken.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Mined(Mined),
    Busy(String),
    Unavailable(String),
    WouldRevert(String),
    Reverted(String),
    Pending(String),
}

/// The credits routes' state.
pub struct CreditsState {
    prepaid: Arc<PrepaidState>,
    chain: Arc<dyn CreditsChain>,
    asset: Asset,
    contract: [u8; 20],
    ledger: String,
    clock: Clock,
    proofs: Mutex<BTreeMap<Hash32, u64>>,
    /// Chain reads this minute: (minute, count).
    reads: Mutex<(u64, u32)>,
    reads_per_minute: u32,
    /// Requests per minute from one client (an IPv4 address or an IPv6 /64 and its wider
    /// prefixes), across these routes.
    per_client_per_minute: u32,
    rate: crate::cache::Caches,
    relay: Option<Relay>,
}

impl CreditsState {
    /// Credits of the `OnyxPrepaidV2` at `contract` (its vouchers in `prepaid`), read through
    /// `chain`, counted in `asset`.
    pub fn new(
        prepaid: Arc<PrepaidState>,
        chain: Arc<dyn CreditsChain>,
        asset: Asset,
        contract: [u8; 20],
        clock: Clock,
    ) -> CreditsState {
        let ledger = format!("eip155:{}:0x{}", asset.chain_id, hex::encode(contract));
        CreditsState {
            prepaid,
            chain,
            asset,
            contract,
            ledger,
            clock,
            proofs: Mutex::new(BTreeMap::new()),
            reads: Mutex::new((0, 0)),
            reads_per_minute: 600,
            per_client_per_minute: 30,
            rate: crate::cache::Caches::new(1, 1),
            relay: None,
        }
    }

    /// The ledger label account proofs sign (`eip155:<chain>:<contract>`).
    pub fn ledger(&self) -> &str {
        &self.ledger
    }

    /// At most `n` chain reads a minute for these routes (default 600); beyond, `503`.
    pub fn with_reads_per_minute(mut self, n: u32) -> Self {
        self.reads_per_minute = n.max(1);
        self
    }

    /// Relay self-signed actions with `relay`. A relay hold left pending by a stop is released
    /// first (its outcome is unknown: an action mined without its fee is the operator's loss,
    /// never the account's). Blocking (journal writes).
    pub fn with_relay(mut self, relay: Relay) -> Result<Self, String> {
        if relay.policy.hold_secs == 0 || relay.policy.hold_secs > self.prepaid.hold_limit {
            return Err(format!(
                "relay holds of {} s need a prepaid hold limit of at least that ({} s)",
                relay.policy.hold_secs, self.prepaid.hold_limit
            ));
        }
        let now = (self.clock)();
        for h in self.prepaid.holds() {
            if h.metric == RELAY_HOLD_METRIC {
                self.prepaid
                    .release_hold(&h.ticket, now)
                    .map_err(|e| format!("releasing a stale relay hold: {e}"))?;
            }
        }
        self.relay = Some(relay);
        Ok(self)
    }

    /// At most `n` requests a minute from one client on these routes (default 30).
    pub fn with_per_client_per_minute(mut self, n: u32) -> Self {
        self.per_client_per_minute = n.max(1);
        self
    }

    /// One request of `client`, or 429 `rate_limited`.
    fn limited(&self, client: Option<Extension<ClientKey>>) -> Result<(), ApiError> {
        let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
        let scopes: Vec<(String, u32)> = rate_scopes(&client, self.per_client_per_minute)
            .into_iter()
            .map(|(k, n)| (format!("credits:{k}"), n))
            .collect();
        if self.rate.take_attempts(&scopes) {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many credits requests from this address",
            )
            .with_header("retry-after", "2".into()))
        }
    }

    fn take_read(&self, now: u64) -> bool {
        let Ok(mut r) = self.reads.lock() else {
            return false;
        };
        let minute = now / 60;
        if r.0 != minute {
            *r = (minute, 0);
        }
        if r.1 >= self.reads_per_minute {
            return false;
        }
        r.1 += 1;
        true
    }

    /// Check the request's account proof for `account` and mark it used.
    fn proof(&self, headers: &HeaderMap, account: &[u8; 20], now: u64) -> Result<bool, ApiError> {
        let Some(h) = headers.get(ACCOUNT_PROOF) else {
            return Ok(false);
        };
        let refused = |m: &str| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_account_proof",
                m.to_string(),
            )
        };
        let key = h
            .to_str()
            .map_err(|_| "malformed proof")
            .and_then(|h| verify_account_proof(h, account, &self.ledger, now))
            .map_err(refused)?;
        let mut used = self
            .proofs
            .lock()
            .map_err(|_| unavailable("the proof store is poisoned"))?;
        used.retain(|_, t| now.saturating_sub(*t) <= ACCOUNT_PROOF_SECS + 30);
        if used.contains_key(&key) {
            return Err(refused("this proof was already used"));
        }
        if used.len() >= MAX_USED_PROOFS {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "credits_busy",
                "too many account reads right now; retry in a minute",
            ));
        }
        used.insert(key, now);
        Ok(true)
    }
}

fn unavailable(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "credits_unavailable", m)
}

fn no_store(status: StatusCode, v: Value) -> Response {
    let mut r = (status, Json(v)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn parse_account(s: &str) -> Result<[u8; 20], ApiError> {
    s.strip_prefix("0x")
        .filter(|h| h.len() == 40 && !h.bytes().any(|c| c.is_ascii_uppercase()))
        .and_then(|h| hex::decode(h).ok())
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_account",
                "the account must be a lowercase 0x address",
            )
        })
}

/// The credits read routes, to merge into [`crate::router_with`]'s `extra`.
pub fn routes(state: Arc<CreditsState>) -> Router {
    Router::new()
        .route("/v1/credits/schedule", get(schedule))
        .route("/v1/credits/{account}", get(account))
        .route("/v1/credits/{account}/lines", get(lines))
        .with_state(state)
}

/// The relay route (a POST: the proxy's bounded POST group), merged into `extra` too.
pub fn relay_routes(state: Arc<CreditsState>) -> Router {
    Router::new()
        .route("/v1/relay", post(relay))
        .with_state(state)
}

async fn schedule(State(st): State<Arc<CreditsState>>) -> Response {
    let (relayer, actions, hash, limits) = match &st.relay {
        Some(r) => {
            let offered: Vec<(RelayKind, u128)> = RelayKind::ALL
                .into_iter()
                .filter_map(|k| r.fee(k).map(|f| (k, f)))
                .collect();
            let shown = FeeSchedule { fees: offered };
            (
                json!(format!("0x{}", hex::encode(r.sender.address()))),
                shown.json(),
                json!(r.schedule.hash().to_string()),
                json!({
                    "perAccountPerHour": r.policy.per_account_per_hour,
                    "failedPerDay": r.policy.reverts_per_day,
                    "dailyGasBudget": r.policy.daily_gas_budget,
                    "accountGasPerDay": r.policy.account_gas_per_day,
                    "maxBaseFeeWei": r.policy.max_base_fee.to_string(),
                }),
            )
        }
        None => (Value::Null, json!([]), Value::Null, Value::Null),
    };
    no_store(
        StatusCode::OK,
        json!({
            "asset": st.asset.json(),
            "ledger": st.ledger,
            "relay": st.relay.is_some(),
            "relayer": relayer,
            "scheduleHash": hash,
            "actions": actions,
            "limits": limits,
        }),
    )
}

async fn account(
    State(st): State<Arc<CreditsState>>,
    Path(account): Path<String>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = st.limited(client) {
        return e.into_response();
    }
    match account_view(&st, &account, &headers).await {
        Ok(v) => no_store(StatusCode::OK, v),
        Err(e) => e.into_response(),
    }
}

async fn account_view(
    st: &CreditsState,
    account: &str,
    headers: &HeaderMap,
) -> Result<Value, ApiError> {
    let who = parse_account(account)?;
    let now = (st.clock)();
    let own = st.proof(headers, &who, now)?;
    if !st.take_read(now) {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "credits_busy",
            "too many account reads right now; retry in a minute",
        ));
    }
    let a = st.chain.account(who).await.map_err(unavailable)?;
    let mut v = json!({
        "account": format!("0x{}", hex::encode(who)),
        "asset": st.asset.json(),
        "view": "public",
        "deposited": a.deposited.to_string(),
        "redeemed": a.redeemed.to_string(),
        "exitAt": a.exit_at,
    });
    if !own {
        return Ok(v);
    }
    let arrivals = st.chain.arrivals(who).await.map_err(unavailable)?;
    let mut by: BTreeMap<&str, u128> = ["deposit", "earned", "refund", "payout"]
        .into_iter()
        .map(|k| (k, 0))
        .collect();
    for x in &arrivals {
        let e = by.entry(origin_name(x.origin)).or_insert(0);
        *e = e.saturating_add(x.amount);
    }
    // The journal's view on the chain's account just read (refresh is rate-limited by max_age).
    st.prepaid.refresh(who, now).await.map_err(unavailable)?;
    let book = st.prepaid.book(&who).unwrap_or_default();
    let held = book
        .held
        .as_ref()
        .map_or(0, |h| h.cumulative.saturating_sub(book.total()));
    v["view"] = json!("account");
    v["credited"] = json!(by
        .iter()
        .map(|(k, n)| (k.to_string(), json!(n.to_string())))
        .collect::<serde_json::Map<_, _>>());
    v["spent"] = json!(book.total().to_string());
    v["held"] = json!(held.to_string());
    v["spendable"] = json!(book.spendable().to_string());
    Ok(v)
}

/// `/lines` query.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinesQuery {
    after: Option<usize>,
    limit: Option<usize>,
}

async fn lines(
    State(st): State<Arc<CreditsState>>,
    Path(account): Path<String>,
    q: Result<Query<LinesQuery>, axum::extract::rejection::QueryRejection>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
) -> Response {
    let r = async {
        st.limited(client)?;
        let who = parse_account(&account)?;
        let Ok(Query(q)) = q else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "after and limit must be numbers",
            ));
        };
        let now = (st.clock)();
        if !st.proof(&headers, &who, now)? {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "account_proof_required",
                "an account's lines are shown only with its own fresh proof (onyx-account-proof)",
            ));
        }
        if !st.take_read(now) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "credits_busy",
                "too many account reads right now; retry in a minute",
            ));
        }
        let limit = q.limit.unwrap_or(MAX_LINES).clamp(1, MAX_LINES);
        let pp = st.prepaid.clone();
        let after = q.after;
        let (ls, next) = tokio::task::spawn_blocking(move || pp.account_lines(who, after, limit))
            .await
            .map_err(|_| unavailable("the journal read failed"))?
            .map_err(unavailable)?;
        let arrivals = st.chain.arrivals(who).await.map_err(unavailable)?;
        Ok(json!({
            "account": format!("0x{}", hex::encode(who)),
            "asset": st.asset.json(),
            "lines": ls.iter().map(|l| json!({
                "seq": l.seq,
                "kind": l.kind.name(),
                "action": l.action,
                "amount": l.amount.to_string(),
                "cumulative": l.cumulative.to_string(),
                "reference": l.reference.to_string(),
                "at": l.at,
            })).collect::<Vec<_>>(),
            "next": next,
            "arrivals": arrivals.iter().map(|a| json!({
                "origin": origin_name(a.origin),
                "amount": a.amount.to_string(),
                "block": a.block,
                "tx": a.tx.to_string(),
            })).collect::<Vec<_>>(),
        }))
    }
    .await;
    match r {
        Ok(v) => no_store(StatusCode::OK, v),
        Err(e) => e.into_response(),
    }
}

async fn relay(
    State(st): State<Arc<CreditsState>>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = st.limited(client) {
        return e.into_response();
    }
    // The work runs in its own task: a hold, once taken, is always charged or released, even
    // if the request's deadline drops this handler.
    let task = tokio::spawn(async move { relay_inner(&st, &headers, &body).await });
    match task.await {
        Ok(Ok(v)) => no_store(StatusCode::OK, v),
        Ok(Err(e)) => e.into_response(),
        Err(_) => crate::internal("the relay task failed").into_response(),
    }
}

async fn relay_inner(
    st: &CreditsState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Value, ApiError> {
    let Some(r) = &st.relay else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "relay_not_offered",
            "this server relays no actions",
        ));
    };
    let invalid = |m: String| ApiError::new(StatusCode::BAD_REQUEST, "invalid_relay", m);
    if body.len() > MAX_RELAY_BODY {
        return Err(invalid(format!("the body is above {MAX_RELAY_BODY} bytes")));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
    let action = RelayAction::parse(&v).map_err(|e| invalid(e.to_string()))?;
    let kind = action.kind();
    let Some(fee) = r.fee(kind) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "relay_not_offered",
            format!("{} is not relayed here", kind.name()),
        ));
    };
    // CL6-2: only the actor's own signed action, checked before anything else.
    action
        .check_signature(r.token_domain.as_ref(), &st.contract)
        .map_err(|e| ApiError::new(StatusCode::FORBIDDEN, "relay_not_signed", e.to_string()))?;
    let Some(h) = headers.get(prepaid_gate::PREPAID_VOUCHER) else {
        return Err(ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "relay_fee_required",
            "a relay is paid with an onyx-prepaid-voucher adding exactly the action's fee",
        )
        .with_field("fee", json!(fee.to_string()))
        .with_field("asset", st.asset.json()));
    };
    let voucher: Voucher = h
        .to_str()
        .ok()
        .and_then(prepaid_gate::parse_voucher)
        .ok_or_else(|| invalid("the onyx-prepaid-voucher header is malformed".into()))?;
    let actor = action.actor();
    if voucher.buyer != actor {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "relay_payer_mismatch",
            "the voucher must be the actor's own: an account pays only for its own actions",
        ));
    }
    // Review CL6-R1-4/5: the voucher's own signature before anything about the account is
    // counted or shown.
    if !voucher.signed(&st.prepaid.domain()) {
        return Err(ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "prepaid_refused",
            crate::prepaid::PrepaidError::NotSigned.to_string(),
        ));
    }
    let now = (st.clock)();
    // Review CL6-R1-3: a deposit authorization that is not valid yet, or would expire before
    // its transaction can be mined, would revert on chain at the relayer's cost.
    if let RelayAction::Deposit {
        valid_after,
        valid_before,
        ..
    } = &action
    {
        if *valid_after >= now || *valid_before < now.saturating_add(r.policy.hold_secs + 60) {
            return Err(invalid(format!(
                "the authorization must be valid now and for at least {} s more",
                r.policy.hold_secs + 60
            )));
        }
    }
    st.prepaid
        .refresh(actor, now)
        .await
        .map_err(|_| crate::prepaid_unavailable())?;
    let data = action.calldata();
    let to = match kind {
        RelayKind::Deposit => st.contract,
        _ => r.registry.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "relay_not_offered",
                "the registry is not configured",
            )
        })?,
    };
    let mut t = b"onyx-relay-v1".to_vec();
    t.extend_from_slice(&actor);
    t.extend_from_slice(&voucher.cumulative.to_be_bytes());
    t.extend_from_slice(&keccak256(&data).0);
    let ticket = keccak256(&t);
    let held = Held {
        ticket,
        metric: RELAY_HOLD_METRIC.into(),
        subject: kind.name().into(),
        deadline: now + r.policy.hold_secs,
        created: now,
    };
    let (pp, incoming, v2) = (st.prepaid.clone(), action.incoming(), voucher.clone());
    match tokio::task::spawn_blocking(move || pp.hold_incoming(v2, fee, held, now, incoming)).await
    {
        Ok(Ok(())) => {}
        Ok(Err(ChargeError::Refused(e))) => {
            let mut err = ApiError::new(
                StatusCode::PAYMENT_REQUIRED,
                "prepaid_refused",
                e.to_string(),
            );
            if let crate::prepaid::PrepaidError::WrongTotal { last, price } = e {
                err = err
                    .with_field("last", json!(last.to_string()))
                    .with_field("price", json!(price.to_string()));
            }
            return Err(err);
        }
        Ok(Err(ChargeError::Unavailable)) | Err(_) => return Err(crate::prepaid_unavailable()),
    }
    // CL6-4: counted once the account's own voucher is held, before anything is simulated or
    // sent; refused, the hold is released.
    if let Err(m) = r.admit(actor, now) {
        let pp = st.prepaid.clone();
        let _ = tokio::task::spawn_blocking(move || pp.release_hold(&ticket, now)).await;
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "relay_rate_limited",
            m,
        ));
    }
    // From here the hold is always charged or released.
    let outcome = execute(r, actor, to, data, now).await;
    let pp = st.prepaid.clone();
    let later = (st.clock)();
    if let Outcome::Mined(m) = outcome {
        let charged = tokio::task::spawn_blocking(move || {
            pp.commit_hold(
                &ticket,
                (prepaid_gate::GAS_ROUND, prepaid_gate::gas_shard()),
                later,
            )
        })
        .await
        .is_ok_and(|c| c == Ok(true));
        let book = st.prepaid.book(&actor).unwrap_or_default();
        return Ok(json!({
            "action": kind.name(),
            "tx": m.hash,
            "block": m.block,
            "gasUsed": m.gas_used,
            "fee": fee.to_string(),
            "charged": charged,
            "total": book.total().to_string(),
            "spendable": book.spendable().to_string(),
        }));
    }
    let _ = tokio::task::spawn_blocking(move || pp.release_hold(&ticket, later)).await;
    Err(match outcome {
        Outcome::Mined(_) => crate::internal("unreachable"),
        Outcome::Busy(m) => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "relay_busy", m),
        Outcome::Unavailable(m) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_busy",
            format!("the chain could not be reached; nothing was charged ({m})"),
        ),
        Outcome::WouldRevert(m) => {
            r.failed(actor, now);
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "relay_would_revert",
                format!("the call would revert; nothing was sent or charged ({m})"),
            )
        }
        Outcome::Reverted(h) => {
            r.failed(actor, now);
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "relay_reverted",
                format!("transaction {h} reverted; nothing was charged"),
            )
            .with_field("tx", json!(h))
        }
        Outcome::Pending(h) => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "relay_pending",
            format!("transaction {h} was not mined in time; nothing was charged"),
        )
        .with_field("tx", json!(h)),
    })
}

/// Base fee cap, simulation, gas budget, send (in that order; nothing is sent unless every
/// check passed).
async fn execute(r: &Relay, who: [u8; 20], to: [u8; 20], data: Vec<u8>, now: u64) -> Outcome {
    match r.sender.base_fee().await {
        Ok(b) if b > r.policy.max_base_fee => {
            return Outcome::Busy(format!(
                "the base fee {b} wei is above the relay's cap {} wei; retry later",
                r.policy.max_base_fee
            ))
        }
        Ok(_) => {}
        Err(e) => return Outcome::Unavailable(e),
    }
    let gas = match r.sender.simulate(to, data.clone()).await {
        Ok(g) => g,
        Err(SimError::Revert(m)) => return Outcome::WouldRevert(m),
        Err(SimError::Rpc(m)) => return Outcome::Unavailable(m),
    };
    let reserved = gas.saturating_mul(5) / 4;
    if let Err(m) = r.reserve(who, reserved, now) {
        return Outcome::Busy(m.into());
    }
    match r.sender.send(to, data, r.policy.send_timeout).await {
        Ok(m) => {
            r.settle(who, reserved, Some(m.gas_used), now);
            Outcome::Mined(m)
        }
        Err(SendError::Reverted(h)) => Outcome::Reverted(h),
        Err(SendError::Pending(h)) => Outcome::Pending(h),
        Err(SendError::WouldRevert(m)) => {
            r.settle(who, reserved, None, now);
            Outcome::WouldRevert(m)
        }
        Err(e @ (SendError::Busy | SendError::Stuck(_) | SendError::Rpc(_))) => {
            r.settle(who, reserved, None, now);
            Outcome::Unavailable(e.to_string())
        }
    }
}

#[cfg(test)]
#[path = "credits_tests.rs"]
mod tests;
