//! Contributor pass: "add data, get data" (ADR-0025 item 3, docs/spec/revenue.md).
//! An address with accepted work (a miner whose root became final, a validator
//! who agreed with it) inside the window reads fresh data free, or at a
//! discount. The request carries an `ONYX-CONTRIBUTOR` header: base64 JSON
//! `{address, issued, signature}`, where `signature` is an EIP-191 signature
//! of [`message`] for this exact resource, time and ledger. Each proof is used
//! once per server process (the used-proof table is per replica: behind N
//! replicas a proof can be honoured up to N times inside its skew window);
//! each address has its own read budget.

use crate::roots::{JsonRpc, LedgerKind, RootError};
use crate::x402::{self, BoxFuture};
use onyx_core::{keccak256, Hash32};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

/// Request header that carries a contributor proof.
pub const CONTRIBUTOR_HEADER: &str = "onyx-contributor";

/// Proofs kept for replay checks; beyond this a new proof is refused (fail
/// closed: the client pays instead).
pub const MAX_PROOFS_TRACKED: usize = 200_000;

/// Addresses remembered as "not a contributor" (REV-L2-F2), bounded.
pub const MAX_NEGATIVE_TRACKED: usize = 65_536;

/// How long a "not a contributor" answer is reused, in seconds (REV-L2-F2).
/// New accepted work counts at most this long after it lands.
pub const NEGATIVE_TTL_SECS: u64 = 60;

/// How long [`ChainContributors`] reuses the node's block number.
pub const HEAD_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// The header's content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributorProof {
    /// `0x` + 40 hex: the contributor.
    pub address: String,
    /// Unix seconds the proof was made.
    pub issued: u64,
    /// `0x` + 130 hex: EIP-191 signature of [`message`].
    pub signature: String,
}

/// The ledger a proof is bound to: `off-chain`, or `eip155:<chain>:<contract>`.
pub fn ledger_label(ledger: LedgerKind) -> String {
    match ledger {
        LedgerKind::OffChain => "off-chain".into(),
        LedgerKind::Chain { chain_id, contract } => {
            format!("eip155:{chain_id}:0x{}", hex::encode(contract))
        }
    }
}

/// The text a contributor signs: one resource, one time, one ledger.
pub fn message(resource: &str, issued: u64, ledger: &str) -> String {
    format!("onyx-contributor-v1\nresource: {resource}\nissued: {issued}\nledger: {ledger}")
}

fn eip191(msg: &[u8]) -> [u8; 32] {
    let mut b = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    b.extend_from_slice(msg);
    keccak256(&b).0
}

/// A header value proving `sk`'s address for `resource` at `issued` (clients, tests).
pub fn sign(sk: &secp256k1::SecretKey, resource: &str, issued: u64, ledger: &str) -> String {
    let secp = secp256k1::Secp256k1::new();
    let pk = secp256k1::PublicKey::from_secret_key(&secp, sk).serialize_uncompressed();
    let mut address = [0u8; 20];
    address.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
    let digest = eip191(message(resource, issued, ledger).as_bytes());
    let msg = secp256k1::Message::from_digest(digest);
    let (rid, rs) = secp.sign_ecdsa_recoverable(&msg, sk).serialize_compact();
    let mut sig = rs.to_vec();
    sig.push(if rid.to_i32() == 0 { 27 } else { 28 });
    x402::encode_header(&ContributorProof {
        address: format!("0x{}", hex::encode(address)),
        issued,
        signature: format!("0x{}", hex::encode(sig)),
    })
}

/// Half the secp256k1 group order: a canonical signature has `s` at most this
/// (EIP-2), so `(r, s)` and `(r, n - s)` are not two encodings of one proof.
const HALF_N: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let v = hex::decode(s.strip_prefix("0x")?).ok()?;
    v.try_into().ok()
}

/// Check a header for `resource` at `now`: well formed, issued within
/// `max_skew` seconds (and not in the future by more than 30 s), signed by
/// the address it names, with a canonical signature (`v` 27 or 28, low `s`).
/// Returns the address and the proof's replay key: keccak(address, digest),
/// so no re-encoding of one signed message is a second proof (REV-L1-F2).
pub fn verify(
    header: &str,
    resource: &str,
    ledger: &str,
    now: u64,
    max_skew: u64,
) -> Result<([u8; 20], Hash32), &'static str> {
    let p: ContributorProof = x402::decode_header(header).map_err(|_| "malformed proof")?;
    let address: [u8; 20] = parse_hex(&p.address).ok_or("bad address")?;
    let sig: [u8; 65] = parse_hex(&p.signature).ok_or("bad signature")?;
    if p.issued > now.saturating_add(30) || now.saturating_sub(p.issued) > max_skew {
        return Err("proof expired or from the future");
    }
    if !matches!(sig[64], 27 | 28) || sig[32..64] > HALF_N[..] {
        return Err("signature is not canonical");
    }
    let digest = eip191(message(resource, p.issued, ledger).as_bytes());
    match x402::exact_evm::recover(&digest, &sig) {
        Some(a) if a == address => {
            let mut k = address.to_vec();
            k.extend_from_slice(&digest);
            Ok((address, keccak256(&k)))
        }
        _ => Err("signature does not match the address"),
    }
}

/// Where contributor status comes from.
pub trait ContributorSource: Send + Sync {
    /// Whether `who` did accepted work recently enough to read free at `now_unix`.
    fn is_contributor<'a>(
        &'a self,
        who: [u8; 20],
        now_unix: u64,
    ) -> BoxFuture<'a, Result<bool, RootError>>;
}

/// Contributors an indexer records (off-chain ledger, demo, tests): the unix
/// time of each address's newest accepted work, valid for `window_secs`.
pub struct StaticContributors {
    window_secs: u64,
    last: RwLock<BTreeMap<[u8; 20], u64>>,
}

impl StaticContributors {
    /// Empty, with a window of `window_secs` (30 days: 2,592,000).
    pub fn new(window_secs: u64) -> Self {
        StaticContributors {
            window_secs,
            last: RwLock::new(BTreeMap::new()),
        }
    }

    /// Record accepted work by `who` at `accepted_unix` (keeps the newest).
    pub fn insert(&self, who: [u8; 20], accepted_unix: u64) {
        if let Ok(mut m) = self.last.write() {
            let e = m.entry(who).or_insert(accepted_unix);
            *e = (*e).max(accepted_unix);
        }
    }
}

/// The contributor window the free-read rule uses by default: accepted work
/// in the last 30 days (ADR-0025).
pub const DEFAULT_CONTRIBUTOR_WINDOW_SECS: u64 = 30 * 86_400;

impl ContributorSource for StaticContributors {
    fn is_contributor<'a>(
        &'a self,
        who: [u8; 20],
        now_unix: u64,
    ) -> BoxFuture<'a, Result<bool, RootError>> {
        let r = self
            .last
            .read()
            .map(|m| {
                m.get(&who)
                    .is_some_and(|t| now_unix.saturating_sub(*t) < self.window_secs)
            })
            .map_err(|_| RootError::Unavailable("contributor lock".into()));
        Box::pin(async move { r })
    }
}

/// Contributor status from `OnyxRevenue.lastAccepted(address)` on chain: the
/// block of the address's newest registered shard must be fewer than
/// `window_blocks` behind the node's latest block (30 days of 2 s Base blocks:
/// 1,296,000).
pub struct ChainContributors<T: JsonRpc> {
    revenue: [u8; 20],
    window_blocks: u64,
    rpc: T,
    /// The node's latest block, reused for [`HEAD_TTL`] (REV-L2-F2).
    head: Mutex<Option<(u64, std::time::Instant)>>,
}

impl<T: JsonRpc> ChainContributors<T> {
    /// Read `OnyxRevenue` at `revenue` through `rpc`.
    pub fn new(revenue: [u8; 20], window_blocks: u64, rpc: T) -> Self {
        ChainContributors {
            revenue,
            window_blocks,
            rpc,
            head: Mutex::new(None),
        }
    }

    /// The node's latest block, from the cache when it is fresh.
    async fn head(&self) -> Result<u64, RootError> {
        let now = std::time::Instant::now();
        if let Some((h, at)) = self.head.lock().ok().and_then(|g| *g) {
            if now.duration_since(at) < HEAD_TTL {
                return Ok(h);
            }
        }
        let h = hex_word_u64(
            &self
                .rpc
                .call("eth_blockNumber", json!([]))
                .await
                .map_err(RootError::Unavailable)?,
        )?;
        if let Ok(mut g) = self.head.lock() {
            *g = Some((h, now));
        }
        Ok(h)
    }
}

fn hex_word_u64(v: &Value) -> Result<u64, RootError> {
    let s = v
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .ok_or_else(|| RootError::Unavailable("bad rpc reply".into()))?;
    let s = s.trim_start_matches('0');
    if s.len() > 16 {
        return Err(RootError::Unavailable("value too large".into()));
    }
    u64::from_str_radix(if s.is_empty() { "0" } else { s }, 16)
        .map_err(|_| RootError::Unavailable("bad rpc reply".into()))
}

/// ABI calldata for `lastAccepted(address)`.
pub fn last_accepted_calldata(who: &[u8; 20]) -> Vec<u8> {
    let mut d = keccak256(b"lastAccepted(address)").0[..4].to_vec();
    d.extend_from_slice(&[0u8; 12]);
    d.extend_from_slice(who);
    d
}

impl<T: JsonRpc> ContributorSource for ChainContributors<T> {
    fn is_contributor<'a>(
        &'a self,
        who: [u8; 20],
        _now_unix: u64,
    ) -> BoxFuture<'a, Result<bool, RootError>> {
        Box::pin(async move {
            let call = json!([{
                "to": format!("0x{}", hex::encode(self.revenue)),
                "data": format!("0x{}", hex::encode(last_accepted_calldata(&who))),
            }, "latest"]);
            let last = hex_word_u64(
                &self
                    .rpc
                    .call("eth_call", call)
                    .await
                    .map_err(RootError::Unavailable)?,
            )?;
            if last == 0 {
                return Ok(false);
            }
            let head = self.head().await?;
            Ok(last != 0 && head.saturating_sub(last) < self.window_blocks)
        })
    }
}

/// How the server honours contributor proofs (`ApiState::with_contributors`).
pub struct ContributorPolicy {
    /// Who is a contributor.
    pub source: Arc<dyn ContributorSource>,
    /// Discount on fresh data, in basis points (10,000: free).
    pub discount_bps: u16,
    /// Contributor reads per address per minute (same burst); 429 beyond.
    pub reads_per_minute: u32,
    /// Contributor rows per address per day (REV-L1-F5): a fixed 24 h
    /// window from the address's first counted read, per replica
    /// (REV-L2-F4); 429 beyond.
    pub rows_per_day: u64,
    /// Proof checks per client address per minute, charged before the
    /// contributor source is asked (REV-L1-F1): throwaway keys cost the
    /// sender's own budget, never a contributor's. At most the payment
    /// attempt budget (REV-L2-F2).
    pub checks_per_minute: u32,
    /// Contributor-source lookups per minute for the whole server, all
    /// clients together (REV-L2-F2): RPC load has a ceiling however many
    /// addresses send proofs. 503 beyond.
    pub source_calls_per_minute: u32,
    /// Oldest proof accepted, in seconds.
    pub max_skew_secs: u64,
    used: Mutex<BTreeMap<Hash32, u64>>,
    /// Addresses the source said are not contributors, with the unix time
    /// of that answer (REV-L2-F2).
    negative: Mutex<BTreeMap<[u8; 20], u64>>,
    /// Global source-lookup bucket: (units, last refill unix second); a
    /// lookup costs 60 units, refill `source_calls_per_minute` per second.
    source_bucket: Mutex<(u64, u64)>,
}

impl ContributorPolicy {
    /// Free reads, 60 per address per minute and 5,000 rows per day, 30
    /// proof checks per client per minute (the default payment-attempt
    /// budget), 600 source lookups per minute server-wide, proofs at most
    /// 120 s old.
    pub fn new(source: Arc<dyn ContributorSource>) -> Self {
        ContributorPolicy {
            source,
            discount_bps: 10_000,
            reads_per_minute: 60,
            rows_per_day: 5_000,
            checks_per_minute: 30,
            source_calls_per_minute: 600,
            max_skew_secs: 120,
            used: Mutex::new(BTreeMap::new()),
            negative: Mutex::new(BTreeMap::new()),
            source_bucket: Mutex::new((u64::MAX, 0)),
        }
    }

    /// Whether a proof was already used (REV-L2-F1): checked before any
    /// budget is charged, so replaying a used proof costs its signer nothing.
    pub fn proof_used(&self, key: &Hash32) -> bool {
        self.used
            .lock()
            .map(|m| m.contains_key(key))
            .unwrap_or(true)
    }

    /// Whether the source said `who` is not a contributor less than
    /// [`NEGATIVE_TTL_SECS`] before `now` (REV-L2-F2).
    pub fn known_not_contributor(&self, who: &[u8; 20], now: u64) -> bool {
        self.negative
            .lock()
            .ok()
            .and_then(|m| m.get(who).copied())
            .is_some_and(|t| now.saturating_sub(t) < NEGATIVE_TTL_SECS)
    }

    /// Remember that the source said `who` is not a contributor at `now`.
    /// Expired answers are dropped when the table is full; if it is still
    /// full the answer is not kept (the global source budget still holds).
    pub fn remember_not_contributor(&self, who: [u8; 20], now: u64) {
        if let Ok(mut m) = self.negative.lock() {
            if m.len() >= MAX_NEGATIVE_TRACKED {
                m.retain(|_, t| now.saturating_sub(*t) < NEGATIVE_TTL_SECS);
            }
            if m.len() < MAX_NEGATIVE_TRACKED || m.contains_key(&who) {
                m.insert(who, now);
            }
        }
    }

    /// Take one lookup from the server-wide source budget at unix time `now`
    /// (REV-L2-F2). False when spent: the caller answers 503.
    pub fn take_source_call(&self, now: u64) -> bool {
        let rate = u64::from(self.source_calls_per_minute.max(1));
        let cap = rate.saturating_mul(60);
        let Ok(mut b) = self.source_bucket.lock() else {
            return false;
        };
        let (units, last) = *b;
        let units = if units == u64::MAX {
            cap
        } else {
            units
                .saturating_add(now.saturating_sub(last).saturating_mul(rate))
                .min(cap)
        };
        let ok = units >= 60;
        *b = (if ok { units - 60 } else { units }, now.max(last));
        ok
    }

    /// The price a contributor pays for something priced `price`.
    pub fn discounted(&self, price: u64) -> u64 {
        let keep = u128::from(10_000u16.saturating_sub(self.discount_bps.min(10_000)));
        u64::try_from(u128::from(price) * keep / 10_000).unwrap_or(u64::MAX)
    }

    /// Mark a proof used (false if it was, or if the table is full). Proofs
    /// older than the skew are dropped first: they would be refused anyway.
    pub fn use_proof(&self, key: Hash32, now: u64) -> bool {
        let Ok(mut m) = self.used.lock() else {
            return false;
        };
        let skew = self.max_skew_secs;
        if m.len() >= MAX_PROOFS_TRACKED {
            m.retain(|_, t| now.saturating_sub(*t) <= skew.saturating_add(30));
        }
        if m.len() >= MAX_PROOFS_TRACKED || m.contains_key(&key) {
            return false;
        }
        m.insert(key, now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The website's run page states the free-read rule from
    /// fixtures/revenue/contributor-policy.json; this keeps that file equal
    /// to the defaults here (AUD-1R, site numbers from data). Regenerate
    /// with `ONYX_BLESS=1 cargo test -p onyx-api contributor_policy_file`.
    #[test]
    fn contributor_policy_file_matches_the_defaults() {
        let p = ContributorPolicy::new(Arc::new(StaticContributors::new(
            DEFAULT_CONTRIBUTOR_WINDOW_SECS,
        )));
        let doc = serde_json::json!({
            "readsPerMinute": p.reads_per_minute,
            "rowsPerDay": p.rows_per_day,
            "discountBps": p.discount_bps,
            "windowDays": DEFAULT_CONTRIBUTOR_WINDOW_SECS / 86_400,
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&doc).expect("json"));
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/revenue/contributor-policy.json"
        );
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::write(path, &text).expect("write");
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/revenue/contributor-policy.json missing; run with ONYX_BLESS=1");
        assert_eq!(on_disk, text, "contributor policy file drifted");
    }

    /// REV-L2-F2: the server-wide lookup budget refills per second and caps
    /// at a minute's worth; negative answers expire after NEGATIVE_TTL_SECS.
    #[test]
    fn source_budget_refills_and_negative_answers_expire() {
        let p = ContributorPolicy::new(Arc::new(StaticContributors::new(60)));
        let mut p = p;
        p.source_calls_per_minute = 2;
        assert!(p.take_source_call(100) && p.take_source_call(100));
        assert!(!p.take_source_call(100), "a minute's worth, then spent");
        assert!(!p.take_source_call(110), "refill is 2 per minute");
        assert!(p.take_source_call(130), "one lookup back after 30 s");
        assert!(p.take_source_call(10_000) && p.take_source_call(10_000));
        assert!(!p.take_source_call(10_000), "never above the cap");
        let who = [7u8; 20];
        assert!(!p.known_not_contributor(&who, 0));
        p.remember_not_contributor(who, 1_000);
        assert!(p.known_not_contributor(&who, 1_000 + NEGATIVE_TTL_SECS - 1));
        assert!(!p.known_not_contributor(&who, 1_000 + NEGATIVE_TTL_SECS));
        let key = Hash32([3; 32]);
        assert!(!p.proof_used(&key));
        assert!(p.use_proof(key, 5));
        assert!(p.proof_used(&key));
    }

    fn key(n: u8) -> secp256k1::SecretKey {
        secp256k1::SecretKey::from_slice(&[n; 32]).expect("key")
    }

    #[test]
    fn proof_binds_address_resource_time_and_ledger() {
        let sk = key(7);
        let h = sign(&sk, "https://x/v1/proofs/1/0xab/0", 1_000, "off-chain");
        let (addr, _) =
            verify(&h, "https://x/v1/proofs/1/0xab/0", "off-chain", 1_010, 120).expect("valid");
        let p: ContributorProof = x402::decode_header(&h).expect("decode");
        assert_eq!(format!("0x{}", hex::encode(addr)), p.address);
        // Another resource, ledger or a late use is refused.
        assert!(verify(&h, "https://x/v1/proofs/1/0xab/1", "off-chain", 1_010, 120).is_err());
        assert!(verify(
            &h,
            "https://x/v1/proofs/1/0xab/0",
            "eip155:1:0x00",
            1_010,
            120
        )
        .is_err());
        assert!(verify(&h, "https://x/v1/proofs/1/0xab/0", "off-chain", 1_200, 120).is_err());
        assert!(verify(&h, "https://x/v1/proofs/1/0xab/0", "off-chain", 900, 120).is_err());
        // Someone else's address with this signature is refused.
        let mut forged = p.clone();
        forged.address = format!("0x{}", hex::encode([9u8; 20]));
        let fh = x402::encode_header(&forged);
        assert!(verify(&fh, "https://x/v1/proofs/1/0xab/0", "off-chain", 1_010, 120).is_err());
    }

    /// REV-L1-F2: the same signed proof re-encoded (v 27 -> 0, or s -> n - s)
    /// is refused, and the replay key does not depend on the encoding.
    #[test]
    fn one_proof_has_one_encoding_and_one_replay_key() {
        const RES: &str = "https://api.onyx.test/v1/proofs/1/0xab/0";
        let h = sign(&key(7), RES, 1_000, "off-chain");
        let p: ContributorProof = x402::decode_header(&h).expect("decode");
        let sig = hex::decode(p.signature.trim_start_matches("0x")).expect("hex");
        let reencode = |sig: &[u8]| {
            x402::encode_header(&ContributorProof {
                signature: format!("0x{}", hex::encode(sig)),
                ..p.clone()
            })
        };
        let (_, k1) = verify(&h, RES, "off-chain", 1_010, 120).expect("original");
        let mut v_alias = sig.clone();
        v_alias[64] -= 27;
        assert_eq!(
            verify(&reencode(&v_alias), RES, "off-chain", 1_010, 120),
            Err("signature is not canonical")
        );
        // High s: n - s, with the other recovery id.
        let n: [u8; 32] =
            hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
                .expect("hex")
                .try_into()
                .expect("32");
        let mut high = sig.clone();
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(n[i]) - i16::from(sig[32 + i]) - borrow;
            borrow = i16::from(d < 0);
            high[32 + i] = (d + 256 * borrow) as u8;
        }
        high[64] = if sig[64] == 27 { 28 } else { 27 };
        assert_eq!(
            verify(&reencode(&high), RES, "off-chain", 1_010, 120),
            Err("signature is not canonical")
        );
        // The key is keccak(address, digest): a second signature of the same
        // message by the same key would collide with the first.
        let (_, k2) = verify(
            &sign(&key(7), RES, 1_000, "off-chain"),
            RES,
            "off-chain",
            1_010,
            120,
        )
        .expect("again");
        assert_eq!(k1, k2);
        let pol = ContributorPolicy::new(Arc::new(StaticContributors::new(100)));
        assert!(pol.use_proof(k1, 1_010));
        assert!(!pol.use_proof(k2, 1_010), "one signed message is one read");
    }

    #[test]
    fn a_proof_is_used_once_and_discounts_round_down() {
        let p = ContributorPolicy::new(Arc::new(StaticContributors::new(100)));
        let k = keccak256(b"proof");
        assert!(p.use_proof(k, 10));
        assert!(!p.use_proof(k, 11));
        assert_eq!(p.discounted(2_500), 0);
        let half = ContributorPolicy {
            discount_bps: 5_000,
            ..ContributorPolicy::new(Arc::new(StaticContributors::new(100)))
        };
        assert_eq!(half.discounted(2_501), 1_250);
    }

    #[test]
    fn static_contributors_expire_after_the_window() {
        let s = StaticContributors::new(100);
        s.insert([1; 20], 1_000);
        s.insert([1; 20], 900);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("rt");
        assert!(rt.block_on(s.is_contributor([1; 20], 1_099)).expect("ok"));
        assert!(!rt.block_on(s.is_contributor([1; 20], 1_100)).expect("ok"));
        assert!(!rt.block_on(s.is_contributor([2; 20], 1_000)).expect("ok"));
    }

    #[test]
    fn last_accepted_selector_is_pinned() {
        // `cast sig "lastAccepted(address)"`
        let d = last_accepted_calldata(&[0xaa; 20]);
        assert_eq!(hex::encode(&d[..4]), "d9b45da2");
        assert_eq!(d.len(), 36);
        assert_eq!(&d[16..], &[0xaa; 20]);
    }
}
