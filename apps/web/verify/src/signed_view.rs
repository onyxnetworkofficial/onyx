//! Level-3 signed facts in the browser (ROADMAP C61, ADR-0028 amendment 8,
//! docs/spec/proof-ladder.md R-SIG-CL-1).
//!
//! A `GET /v1/signed/{fact}` body is the operator's word. The client does not
//! take it: [`check`] recomputes everything from the signed payload and
//! compares it with what the client read ITSELF from `OnyxSignedBook` at a
//! finalized block of its own RPC (the browser: a public Base Sepolia RPC the
//! reader can change; no API key). Refused unless all of this holds:
//!
//! 1. the body has exactly the keys the API's `/v1/signed` route writes, `level`
//!    3, and names the client's pinned chain and book;
//! 2. `payload` is a canonical Borsh `SignedPayload`; the 65-byte signature
//!    is canonical EIP-191 over its text, and the payload's `key` is the id of
//!    (its source, the recovered signer), so no other key could have signed;
//! 3. `fact`, `reportHash` and `valueHash` are recomputed from payload and
//!    signature, and every plain field (source, key, metric, subject, value,
//!    time, sequence) is the payload's;
//! 4. the client's RPC is on the pinned chain; its finalized block is fresh
//!    (at most [`MAX_FINALIZED_AGE_SECS`] old on the client's clock, never
//!    more than [`MAX_BLOCK_AHEAD_SECS`] ahead) and the code at the pinned
//!    address at that block hashes to the pinned build;
//! 5. at that block, `settleable(fact)` names exactly this report and value
//!    hash, and `factView(fact)` is accepted on this report at the body's
//!    `acceptedAt`;
//! 6. that block's time and the client's own clock are both at least
//!    `acceptedAt + 5,400` s (the serving margin over the sequencer drift,
//!    THREATS L3C-12, C10-R1-2).
//!
//! No live book is deployed yet (ROADMAP C10, lead-pending). The website shows
//! one answer recorded in the local-anvil rehearsal (`scripts/level3-book.sh
//! local`) with the chain answers recorded at the same moment: the book and
//! those answers are compiled in below ([`rehearsal`]), so an edited file is
//! refused. The page labels it a rehearsal everywhere.
//!
//! Not money: a signed fact settles no market here (ROADMAP H6).

use borsh::BorshSerialize;
use onyx_core::signed::{KEY_DOMAIN, PAYLOAD_DOMAIN};
use onyx_core::{
    from_canonical_bytes, keccak256, Hash32, SignedPayload, SignedReport, SourceId, Subject,
    Value as OnyxValue,
};
use onyx_labels::attest::eip191_recover;
use serde_json::{json, Map, Value};
use wasm_bindgen::prelude::*;

/// The dispute window of `OnyxSignedBook` (`DISPUTE_WINDOW_SECS`).
pub const DISPUTE_WINDOW_SECS: u64 = 3_600;
/// The serving margin: the window plus the OP Stack sequencer drift
/// (the `/v1/signed` route's `SERVABLE_AFTER_SECS`; pinned by a test).
pub const SERVABLE_AFTER_SECS: u64 = 5_400;
/// The oldest finalized block the client takes, on its own clock: Base
/// Sepolia's finalized head ran up to 1,004 blocks (about 2,008 s) behind
/// the head when measured (deploy/health-watch/thresholds.json), so an hour
/// leaves room for that lag and no more (review C61-R1-3: the book disputes a
/// fact whenever a contradiction lands, so every second of tolerated lag is a
/// second a since-disputed fact can still check out). An older block is a
/// lagging or stuck RPC. A RPC that lies about the timestamp is not caught
/// here (C61-4, ROADMAP C63).
pub const MAX_FINALIZED_AGE_SECS: u64 = 3_600;
/// The most a finalized block's time may run ahead of the client's clock:
/// the sequencer drift (1,800 s) plus the clock skew Onyx allows (300 s).
pub const MAX_BLOCK_AHEAD_SECS: u64 = 2_100;
/// Largest body or reads document taken (the book's code is at most 24,576
/// bytes, 49,154 hex characters).
pub const MAX_DOC_BYTES: usize = 256 << 10;

/// The keys of a `/v1/signed` body, all required, no others
/// (the `/v1/signed` route's `SignedStore::answer`; pinned by a test).
pub const BODY_KEYS: [&str; 19] = [
    "level",
    "fact",
    "source",
    "key",
    "metric",
    "subject",
    "value",
    "observedUnix",
    "sequence",
    "payload",
    "signature",
    "reportHash",
    "valueHash",
    "acceptedAt",
    "checkedAt",
    "book",
    "chainId",
    "verifyCall",
    "moneySettlement",
];

/// Keys a `/v1/signed` body may carry beyond [`BODY_KEYS`]: `ladder`
/// (ADR-0048 amendment 2, ROADMAP C64). A node from before it leaves it out;
/// when present it must be exactly [`expected_ladder`].
pub const OPTIONAL_KEYS: [&str; 1] = ["ladder"];

/// The one-step ladder a level-3 body must carry: the value of the signed
/// fact (`fact_key(metric, subject, observedUnix)` and the canonical value
/// hash), level 3 on the report hash, at the book's `acceptedAt` (compared
/// with the book by [`check`]), from `OnyxSignedBook.ReportRecorded`. The
/// node holds no stronger lane for the fact, so no other step is taken.
pub fn expected_ladder(payload: &SignedPayload, report_hash: &Hash32, accepted_at: u64) -> Value {
    let value = onyx_core::ladder::value_key(
        &onyx_core::ladder::fact_key(&payload.metric, &payload.subject, payload.observed_unix),
        &onyx_core::ladder::ladder_value_hash(&payload.value),
    );
    json!({
        "value": value.to_string(),
        "level": 3,
        "steps": [{
            "level": 3,
            "name": "source-proven",
            "evidence": report_hash.to_string(),
            "at": accepted_at,
            "record": "OnyxSignedBook.ReportRecorded",
        }],
    })
}

/// The book a client trusts: chain, address and the keccak of its runtime
/// code (the build with its reviewer filled in, like the node's manifest pin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookPin {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// `OnyxSignedBook`.
    pub book: [u8; 20],
    /// keccak-256 of its runtime code.
    pub code_hash: Hash32,
}

/// The local-anvil rehearsal the website shows (ROADMAP C61): recorded by
/// `scripts/level3-book.sh local` (`apps/web/verify/examples/signed-client.rs
/// --export`) into apps/web/static/level3/signed-rehearsal.json. A local
/// chain (31337) that no longer runs; NOT a live deployment.
pub mod rehearsal {
    /// Chain id of the rehearsal's anvil.
    pub const CHAIN_ID: u64 = 31_337;
    /// The book the rehearsal deployed (dev account 0 deploys the settlement
    /// first; reviewer dev account 1).
    pub const BOOK: &str = "0x5fc8d32690cc91d4c39d9d3abcbd16989f875707";
    /// keccak-256 of its runtime code at the recorded block.
    pub const CODE_HASH: &str =
        "0x9b4da615f513b46d12684cc596fc400882aa9ef975acd170d4296ae4cae79507";
    /// [`super::reads_digest`] of the chain answers recorded with the body.
    pub const READS_DIGEST: &str =
        "0xef07191832d9883cbf03cd5904e9009bc217a2661a0310205fe327dec81472a6";
    /// When the answers were read (host clock, unix seconds: 2026-10-18 UTC).
    pub const RECORDED_UNIX: u64 = 1_792_338_319;
    /// The rehearsal moved the anvil's clock 5,400 s on with
    /// `evm_increaseTime` (the host clock did not move); its node and client
    /// stand in for 90 minutes of host time with this many seconds, as
    /// `examples/level3-book-anvil.rs` does for the node's log.
    pub const CLOCK_STAND_IN_SECS: u64 = 5_460;
}

/// The rehearsal's pin.
pub fn rehearsal_pin() -> Result<BookPin, String> {
    Ok(BookPin {
        chain_id: rehearsal::CHAIN_ID,
        book: addr20(rehearsal::BOOK).ok_or("bad rehearsal book")?,
        code_hash: hash32(rehearsal::CODE_HASH).ok_or("bad rehearsal code hash")?,
    })
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    hex::decode(s.strip_prefix("0x")?).ok()
}

fn addr20(s: &str) -> Option<[u8; 20]> {
    unhex(s)?.try_into().ok()
}

fn hash32(s: &str) -> Option<Hash32> {
    Some(Hash32(unhex(s)?.try_into().ok()?))
}

/// A JSON-RPC quantity (`0x` hex, no leading zeros beyond `0x0`).
fn quantity(v: &Value) -> Option<u64> {
    let s = v.as_str()?.strip_prefix("0x")?;
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// Keccak-256 of `"onyx-signed-fact-v1" ‖ borsh(source, metric, subject,
/// observed_unix)` (`onyx_coordinator::signed::signed_fact_key`; pinned by a
/// test).
pub fn signed_fact_key(p: &SignedPayload) -> Hash32 {
    let mut b = b"onyx-signed-fact-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&(&p.source, &p.metric, &p.subject, p.observed_unix)).unwrap_or_default(),
    );
    keccak256(&b)
}

/// `keccak256(borsh(value))` (`onyx_coordinator::signed_chain::value_hash`).
pub fn value_hash(v: &OnyxValue) -> Hash32 {
    keccak256(&borsh::to_vec(v).unwrap_or_default())
}

/// `SourceKey::id` of (source, signer).
pub fn key_id(source: &SourceId, signer: &[u8; 20]) -> Hash32 {
    let mut b = KEY_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&(source, signer)).unwrap_or_default());
    keccak256(&b)
}

/// The served JSON form of a value (the `/v1/signed` route's `value_json`).
pub fn value_json(v: &OnyxValue) -> Value {
    match v {
        OnyxValue::Int(i) => json!({"int": i.to_string()}),
        OnyxValue::UInt(u) => json!({"uint": u.to_string()}),
        OnyxValue::Fixed { mantissa, scale } => {
            json!({"fixed": {"mantissa": mantissa.to_string(), "scale": scale}})
        }
        OnyxValue::Bool(b) => json!({"bool": b}),
        OnyxValue::Bytes(b) => json!({"bytes": hx(b)}),
    }
}

/// The value as the page prints it: an integer in digits, a fixed-point
/// value with its decimals (string math, no floats).
pub fn value_text(v: &OnyxValue) -> String {
    match v {
        OnyxValue::Int(i) => i.to_string(),
        OnyxValue::UInt(u) => u.to_string(),
        OnyxValue::Fixed { mantissa, scale } => {
            let neg = *mantissa < 0;
            let digits = mantissa.unsigned_abs().to_string();
            let s = usize::from(*scale);
            let padded = format!("{digits:0>width$}", width = s + 1);
            let (i, f) = padded.split_at(padded.len() - s);
            format!("{}{i}.{f}", if neg { "-" } else { "" })
        }
        OnyxValue::Bool(b) => b.to_string(),
        OnyxValue::Bytes(b) => hx(b),
    }
}

fn subject_parts(s: &Subject) -> (&'static str, &str) {
    match s {
        Subject::Entity(x) => ("entity", x),
        Subject::Address(x) => ("address", x),
        Subject::Instrument(x) => ("instrument", x),
        Subject::Aggregate(x) => ("aggregate", x),
    }
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

fn calldata(sig: &str, word: &Hash32) -> String {
    let mut d = selector(sig).to_vec();
    d.extend_from_slice(&word.0);
    hx(&d)
}

/// The three reads the client makes at its finalized block, for `fact`:
/// `eth_getCode(book)`, `settleable(fact)` and `factView(fact)` (calldata).
pub fn calls(pin: &BookPin, fact: &Hash32) -> Value {
    json!({
        "to": hx(&pin.book),
        "settleable": calldata("settleable(bytes32)", fact),
        "factView": calldata("factView(bytes32)", fact),
    })
}

fn word(b: &[u8], i: usize) -> Option<[u8; 32]> {
    b.get(i * 32..i * 32 + 32)?.try_into().ok()
}

fn word_u64(w: &[u8; 32]) -> Option<u64> {
    if w[..24].iter().any(|x| *x != 0) {
        return None;
    }
    Some(u64::from_be_bytes(w[24..].try_into().ok()?))
}

/// `settleable(bytes32)`'s return: (ok, report hash, value hash).
/// Exactly three words (review C61-R1-6).
fn decode_settleable(s: &[u8]) -> Option<(u64, Hash32, Hash32)> {
    if s.len() != 96 {
        return None;
    }
    let ok = word_u64(&word(s, 0)?)?;
    Some((ok, Hash32(word(s, 1)?), Hash32(word(s, 2)?)))
}

/// `factView(bytes32)`'s head: (state, accepted report hash, acceptedAt).
/// The ABI layout exactly: four head words, the dispute array at offset
/// 0x80, its length and that many words, nothing after (review C61-R1-6).
fn decode_fact_view(f: &[u8]) -> Option<(u64, Hash32, u64)> {
    if word_u64(&word(f, 3)?)? != 128 {
        return None;
    }
    let n = usize::try_from(word_u64(&word(f, 4)?)?).ok()?;
    if f.len() != 160usize.checked_add(n.checked_mul(32)?)? {
        return None;
    }
    Some((
        word_u64(&word(f, 0)?)?,
        Hash32(word(f, 1)?),
        word_u64(&word(f, 2)?)?,
    ))
}

/// The chain answers, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reads {
    /// `eth_chainId`.
    pub chain_id: u64,
    /// The finalized block's number.
    pub number: u64,
    /// Its hash (every other read is made at it, EIP-1898).
    pub hash: Hash32,
    /// Its timestamp.
    pub time: u64,
    /// keccak-256 of `eth_getCode(book)` at that block.
    pub code_hash: Hash32,
    /// `settleable(fact)`'s return bytes.
    pub settleable: Vec<u8>,
    /// `factView(fact)`'s return bytes.
    pub fact_view: Vec<u8>,
}

/// The Borsh form [`reads_digest`] hashes (ADR-0002).
#[derive(BorshSerialize)]
struct ReadsWire<'a> {
    chain_id: u64,
    number: u64,
    hash: [u8; 32],
    time: u64,
    code_hash: [u8; 32],
    settleable: &'a [u8],
    fact_view: &'a [u8],
}

/// `keccak256("onyx-signed-reads-v1" ‖ borsh(reads))`: what the rehearsal
/// pins, so a recorded answer cannot be edited.
pub fn reads_digest(r: &Reads) -> Hash32 {
    let mut b = b"onyx-signed-reads-v1".to_vec();
    b.extend_from_slice(
        &borsh::to_vec(&ReadsWire {
            chain_id: r.chain_id,
            number: r.number,
            hash: r.hash.0,
            time: r.time,
            code_hash: r.code_hash.0,
            settleable: &r.settleable,
            fact_view: &r.fact_view,
        })
        .unwrap_or_default(),
    );
    keccak256(&b)
}

/// Decode the reads JSON: `{"chainId": "0x..", "finalized": {"number",
/// "hash", "timestamp"}, "code": "0x..", "settleable": "0x..", "factView":
/// "0x.."}` (raw JSON-RPC results).
pub fn decode_reads(v: &Value) -> Result<Reads, String> {
    let o = v.as_object().ok_or("chain reads: not an object")?;
    for k in o.keys() {
        if !["chainId", "finalized", "code", "settleable", "factView"].contains(&k.as_str()) {
            return Err(format!("chain reads: unexpected key {k}"));
        }
    }
    let chain_id = quantity(&v["chainId"]).ok_or("eth_chainId: not a quantity")?;
    let b = &v["finalized"];
    if b.is_null() {
        return Err(
            "your RPC has no finalized block: level 3 is checked only at a finalized block".into(),
        );
    }
    let number = quantity(&b["number"]).ok_or("finalized block: bad number")?;
    let time = quantity(&b["timestamp"]).ok_or("finalized block: bad timestamp")?;
    let hash = b["hash"]
        .as_str()
        .and_then(hash32)
        .ok_or("finalized block: bad hash")?;
    let raw = |k: &str| -> Result<Vec<u8>, String> {
        v[k].as_str()
            .and_then(unhex)
            .ok_or_else(|| format!("{k}: not 0x hex"))
    };
    Ok(Reads {
        chain_id,
        number,
        hash,
        time,
        code_hash: keccak256(&raw("code")?),
        settleable: raw("settleable")?,
        fact_view: raw("factView")?,
    })
}

/// What the client checked: every field the page shows comes from here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The signed payload.
    pub payload: SignedPayload,
    /// The address that signed it.
    pub signer: [u8; 20],
    /// The fact.
    pub fact: Hash32,
    /// The report hash the book settles on.
    pub report_hash: Hash32,
    /// The value hash.
    pub value_hash: Hash32,
    /// When the book accepted it (block time).
    pub accepted_at: u64,
    /// The client's reads.
    pub reads: Reads,
}

impl Checked {
    /// The JSON the page renders.
    pub fn to_json(&self, pin: &BookPin) -> Value {
        let p = &self.payload;
        let (kind, subject) = subject_parts(&p.subject);
        json!({
            "source": p.source.0,
            "metric": p.metric.0,
            "subject_kind": kind,
            "subject": subject,
            "value": value_text(&p.value),
            "value_json": value_json(&p.value),
            "observed_unix": p.observed_unix,
            "sequence": p.sequence,
            "key": p.key.to_string(),
            "signer": hx(&self.signer),
            "fact": self.fact.to_string(),
            "report_hash": self.report_hash.to_string(),
            "value_hash": self.value_hash.to_string(),
            "accepted_at": self.accepted_at,
            "servable_at": self.accepted_at.saturating_add(SERVABLE_AFTER_SECS),
            "block": self.reads.number,
            "block_hash": self.reads.hash.to_string(),
            "block_time": self.reads.time,
            "chain_id": pin.chain_id,
            "book": hx(&pin.book),
            "code_hash": pin.code_hash.to_string(),
        })
    }
}

fn str_of<'a>(body: &'a Map<String, Value>, k: &str, problems: &mut Vec<String>) -> &'a str {
    match body.get(k).and_then(Value::as_str) {
        Some(s) => s,
        None => {
            problems.push(format!("the body's {k} is not a string"));
            ""
        }
    }
}

fn u64_of(body: &Map<String, Value>, k: &str, problems: &mut Vec<String>) -> Option<u64> {
    let v = body.get(k).and_then(Value::as_u64);
    if v.is_none() {
        problems.push(format!("the body's {k} is not a whole number"));
    }
    v
}

/// What the body-only checks found: payload, signer, recomputed hashes and
/// the body's `acceptedAt` (still to be compared with the book).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyChecked {
    /// The signed payload.
    pub payload: SignedPayload,
    /// The address that signed it.
    pub signer: [u8; 20],
    /// The fact.
    pub fact: Hash32,
    /// The report hash.
    pub report_hash: Hash32,
    /// The value hash.
    pub value_hash: Hash32,
    /// The body's `acceptedAt`.
    pub accepted_at: u64,
}

/// The body-only checks (rules 1-3): the payload, its signature and key, and
/// every hash and field recomputed. `Err` holds every problem found.
pub fn check_body(body: &Value, pin: &BookPin) -> Result<BodyChecked, Vec<String>> {
    let mut problems = Vec::new();
    let Some(o) = body.as_object() else {
        return Err(vec!["the body is not a JSON object".into()]);
    };
    for k in o.keys() {
        if !BODY_KEYS.contains(&k.as_str()) && !OPTIONAL_KEYS.contains(&k.as_str()) {
            problems.push(format!(
                "the body carries a key the client does not take: {k}"
            ));
        }
    }
    for k in BODY_KEYS {
        if !o.contains_key(k) {
            problems.push(format!("the body has no {k}"));
        }
    }
    if o.get("level") != Some(&json!(3)) {
        problems.push("the body is not a level-3 answer".into());
    }
    if o.get("chainId").and_then(Value::as_u64) != Some(pin.chain_id) {
        problems.push(format!(
            "the body names another chain than the one this client pins ({})",
            pin.chain_id
        ));
    }
    if str_of(o, "book", &mut problems) != hx(&pin.book) {
        problems.push("the body names another book than the one this client pins".into());
    }
    let payload = match unhex(str_of(o, "payload", &mut problems))
        .ok_or_else(|| "payload: not 0x hex".to_string())
        .and_then(|b| {
            let p: SignedPayload = from_canonical_bytes(&b)
                .map_err(|e| format!("payload: not a signed payload ({e})"))?;
            if borsh::to_vec(&p).ok().as_deref() != Some(b.as_slice()) {
                return Err("payload: not the canonical encoding".into());
            }
            p.validate().map_err(|e| format!("payload: {e}"))?;
            Ok(p)
        }) {
        Ok(p) => p,
        Err(e) => {
            problems.push(e);
            return Err(problems);
        }
    };
    let signature: Option<[u8; 65]> =
        unhex(str_of(o, "signature", &mut problems)).and_then(|b| b.try_into().ok());
    let Some(signature) = signature else {
        problems.push("signature: not 65 bytes of 0x hex".into());
        return Err(problems);
    };
    let message = format!("{PAYLOAD_DOMAIN}:0x{}", payload.digest().to_hex());
    debug_assert_eq!(message, payload.message());
    let signer = eip191_recover(message.as_bytes(), &signature);
    match signer {
        None => problems.push("the signature does not recover (not a canonical EIP-191 signature)".into()),
        Some(s) if key_id(&payload.source, &s) != payload.key => problems.push(
            "the payload's key is not the key of the address that signed it: someone else signed this value".into(),
        ),
        Some(_) => {}
    }
    let report = SignedReport {
        payload: payload.clone(),
        signature,
    };
    let fact = signed_fact_key(&payload);
    let report_hash = report.hash();
    let vh = value_hash(&payload.value);
    let same = |k: &str, want: String, problems: &mut Vec<String>| {
        if o.get(k).and_then(Value::as_str) != Some(want.as_str()) {
            problems.push(format!(
                "the body's {k} is not the one recomputed from the signed payload"
            ));
        }
    };
    same("fact", fact.to_string(), &mut problems);
    same("reportHash", report_hash.to_string(), &mut problems);
    same("valueHash", vh.to_string(), &mut problems);
    same("source", payload.source.0.clone(), &mut problems);
    same("key", payload.key.to_string(), &mut problems);
    same("metric", payload.metric.0.clone(), &mut problems);
    same(
        "subject",
        subject_parts(&payload.subject).1.to_string(),
        &mut problems,
    );
    if o.get("value") != Some(&value_json(&payload.value)) {
        problems.push("the body's value is not the signed value".into());
    }
    if u64_of(o, "observedUnix", &mut problems).is_some_and(|t| t != payload.observed_unix) {
        problems.push("the body's observedUnix is not the signed time".into());
    }
    if u64_of(o, "sequence", &mut problems).is_some_and(|s| s != payload.sequence) {
        problems.push("the body's sequence is not the signed sequence".into());
    }
    let accepted_at = u64_of(o, "acceptedAt", &mut problems).unwrap_or(0);
    if let Some(l) = o.get("ladder") {
        if *l != expected_ladder(&payload, &report_hash, accepted_at) {
            problems.push(
                "the body's ladder is not this report's one step at level 3 (a forged level or history)"
                    .into(),
            );
        }
    }
    match (problems.is_empty(), signer) {
        (true, Some(signer)) => Ok(BodyChecked {
            payload,
            signer,
            fact,
            report_hash,
            value_hash: vh,
            accepted_at,
        }),
        _ => Err(problems),
    }
}

/// The whole client check (rules 1-6): `body` against the client's own
/// `reads` at its finalized block, `now` the client's clock. `Err` holds every
/// problem found; nothing of a refused body is shown.
pub fn check(body: &Value, pin: &BookPin, reads: &Reads, now: u64) -> Result<Checked, Vec<String>> {
    let BodyChecked {
        payload,
        signer,
        fact,
        report_hash,
        value_hash: vh,
        accepted_at,
    } = check_body(body, pin)?;
    let mut problems = Vec::new();
    if reads.chain_id != pin.chain_id {
        problems.push(format!(
            "your RPC is on chain {}, the book this client pins is on chain {}",
            reads.chain_id, pin.chain_id
        ));
        return Err(problems);
    }
    if reads.time > now.saturating_add(MAX_BLOCK_AHEAD_SECS) {
        problems.push(format!(
            "your RPC's finalized block is {} s ahead of your clock (most {MAX_BLOCK_AHEAD_SECS} s)",
            reads.time - now
        ));
    }
    if reads.time.saturating_add(MAX_FINALIZED_AGE_SECS) < now {
        problems.push(format!(
            "your RPC's finalized block is {} s old on your clock (most {MAX_FINALIZED_AGE_SECS} s): a stale or stuck RPC, or your clock is ahead (review C61-R2-3)",
            now - reads.time
        ));
    }
    if reads.code_hash != pin.code_hash {
        problems.push(
            "the code at the book's address is not the pinned OnyxSignedBook build at your finalized block".into(),
        );
    }
    let settle = decode_settleable(&reads.settleable);
    match settle {
        None => problems.push("settleable(): the answer does not decode".into()),
        Some((1, r, v)) => {
            if r != report_hash {
                problems.push("at your finalized block the book settles this fact on another report".into());
            }
            if v != vh {
                problems.push("at your finalized block the book settles this fact on another value".into());
            }
        }
        Some((0, _, _)) => problems.push(
            "at your finalized block the book does not let anyone settle this fact (unknown, disputed or inside its window)".into(),
        ),
        Some(_) => problems.push("settleable(): the answer does not decode".into()),
    }
    let view = decode_fact_view(&reads.fact_view);
    match view {
        None => problems.push("factView(): the answer does not decode".into()),
        Some((state, accepted, at)) => {
            if state != 1 {
                problems.push(match state {
                    2 => "at your finalized block the book holds a contradiction for this fact"
                        .into(),
                    0 => "at your finalized block the book has no value for this fact".into(),
                    _ => "factView(): the answer does not decode".into(),
                });
            }
            if state == 1 && accepted != report_hash {
                problems.push("factView(): the book accepted another report for this fact".into());
            }
            if at != accepted_at {
                problems.push("the body's acceptedAt is not the book's".into());
            }
        }
    }
    let servable_at = accepted_at.saturating_add(SERVABLE_AFTER_SECS);
    if reads.time < servable_at {
        problems.push(format!(
            "at your finalized block the fact is {} s short of the {SERVABLE_AFTER_SECS} s serving margin",
            servable_at - reads.time
        ));
    }
    if now < servable_at {
        problems.push(format!(
            "on your clock the fact is {} s short of the {SERVABLE_AFTER_SECS} s serving margin",
            servable_at - now
        ));
    }
    if problems.is_empty() {
        Ok(Checked {
            payload,
            signer,
            fact,
            report_hash,
            value_hash: vh,
            accepted_at,
            reads: reads.clone(),
        })
    } else {
        Err(problems)
    }
}

/// The page's JSON for a check result.
fn result_json(r: Result<Checked, Vec<String>>, pin: &BookPin, extra: Value) -> String {
    let mut out = match r {
        Ok(c) => {
            let mut v = c.to_json(pin);
            v["ok"] = json!(true);
            v["problems"] = json!([]);
            v
        }
        Err(p) => json!({"ok": false, "problems": p}),
    };
    if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    out.to_string()
}

/// The recorded rehearsal document (apps/web/static/level3/signed-rehearsal.json):
/// `{"kind", "version", "rehearsal": {..}, "body": {..}, "reads": {..}}`.
pub fn check_rehearsal_doc(doc: &Value) -> Result<Checked, Vec<String>> {
    let pin = rehearsal_pin().map_err(|e| vec![e])?;
    if doc["kind"] != "onyx-level3-rehearsal" || doc["version"] != 1 {
        return Err(vec!["not a level-3 rehearsal record (kind, version)".into()]);
    }
    let reads = decode_reads(&doc["reads"]).map_err(|e| vec![e])?;
    let want = hash32(rehearsal::READS_DIGEST).ok_or_else(|| vec!["bad pin".to_string()])?;
    if reads_digest(&reads) != want {
        return Err(vec![
            "the recorded chain answers are not the ones this verifier pins for the rehearsal"
                .into(),
        ]);
    }
    if doc["rehearsal"]["recordedUnix"].as_u64() != Some(rehearsal::RECORDED_UNIX)
        || doc["rehearsal"]["clockStandInSecs"].as_u64() != Some(rehearsal::CLOCK_STAND_IN_SECS)
    {
        return Err(vec![
            "the record's time is not the pinned recording time".into()
        ]);
    }
    check(
        &doc["body"],
        &pin,
        &reads,
        rehearsal::RECORDED_UNIX + rehearsal::CLOCK_STAND_IN_SECS,
    )
}

fn parse(text: &str, what: &str) -> Result<Value, JsError> {
    if text.len() > MAX_DOC_BYTES {
        return Err(JsError::new(&format!("{what} too large")));
    }
    serde_json::from_str(text).map_err(|e| JsError::new(&format!("{what}: {e}")))
}

/// Check the recorded rehearsal (the Data page's level-3 card): the body
/// against the chain answers recorded with it, both pinned here.
#[wasm_bindgen]
pub fn check_signed_rehearsal(doc_json: &str) -> Result<String, JsError> {
    let doc = parse(doc_json, "rehearsal record")?;
    let pin = rehearsal_pin().map_err(|e| JsError::new(&e))?;
    Ok(result_json(
        check_rehearsal_doc(&doc),
        &pin,
        json!({
            "rehearsal": true,
            "recorded_unix": rehearsal::RECORDED_UNIX,
            "clock_stand_in_secs": rehearsal::CLOCK_STAND_IN_SECS,
            "chain_reads": "recorded",
        }),
    ))
}

/// The pin, the fact the body names and the calls the page sends to the
/// reader's RPC at its finalized block (the page computes no ABI itself).
#[wasm_bindgen]
pub fn signed_live_calls(body_json: &str) -> Result<String, JsError> {
    let body = parse(body_json, "level-3 body")?;
    let pin = rehearsal_pin().map_err(|e| JsError::new(&e))?;
    let fact = body["fact"]
        .as_str()
        .and_then(hash32)
        .ok_or_else(|| JsError::new("the body names no fact"))?;
    let mut c = calls(&pin, &fact);
    c["chainId"] = json!(pin.chain_id);
    Ok(c.to_string())
}

/// Check `body` against the reader's own reads (`reads_json`, raw JSON-RPC
/// results from the RPC the reader chose) at `now` (the reader's clock).
/// The pin is the rehearsal's until a live book is deployed (ROADMAP C10):
/// against Base Sepolia it is refused on the chain id, as it must be.
#[wasm_bindgen]
pub fn check_signed_live(body_json: &str, reads_json: &str, now: u64) -> Result<String, JsError> {
    let body = parse(body_json, "level-3 body")?;
    let reads = parse(reads_json, "chain reads")?;
    let pin = rehearsal_pin().map_err(|e| JsError::new(&e))?;
    let r = decode_reads(&reads)
        .map_err(|e| vec![e])
        .and_then(|reads| check(&body, &pin, &reads, now));
    Ok(result_json(
        r,
        &pin,
        json!({"rehearsal": false, "chain_reads": "yours"}),
    ))
}

/// `body` with a forged value: the payload re-encoded with another value and
/// every hash recomputed, the signature kept (what a lying node can do
/// without the source's key).
pub fn forge_value(body: &Value) -> Result<Value, String> {
    let bytes = body["payload"]
        .as_str()
        .and_then(unhex)
        .ok_or("no payload")?;
    let mut p: SignedPayload = from_canonical_bytes(&bytes).map_err(|e| e.to_string())?;
    p.value = match p.value {
        OnyxValue::UInt(u) => OnyxValue::UInt(u.wrapping_add(1)),
        OnyxValue::Int(i) => OnyxValue::Int(i.wrapping_add(1)),
        OnyxValue::Fixed { mantissa, scale } => OnyxValue::Fixed {
            mantissa: mantissa.wrapping_add(1),
            scale,
        },
        OnyxValue::Bool(x) => OnyxValue::Bool(!x),
        OnyxValue::Bytes(mut x) => {
            x.push(0);
            OnyxValue::Bytes(x)
        }
    };
    let signature: [u8; 65] = body["signature"]
        .as_str()
        .and_then(unhex)
        .and_then(|s| s.try_into().ok())
        .ok_or("no signature")?;
    let report = SignedReport {
        payload: p.clone(),
        signature,
    };
    let mut out = body.clone();
    out["payload"] = json!(hx(&borsh::to_vec(&p).unwrap_or_default()));
    out["value"] = value_json(&p.value);
    out["reportHash"] = json!(report.hash().to_string());
    out["valueHash"] = json!(value_hash(&p.value).to_string());
    Ok(out)
}

/// The rehearsal record with a forged body ([`forge_value`]): the page checks
/// it to show that the client's chain read refuses it.
#[wasm_bindgen]
pub fn signed_forged_value(doc_json: &str) -> Result<String, JsError> {
    let mut doc = parse(doc_json, "rehearsal record")?;
    doc["body"] = forge_value(&doc["body"]).map_err(|e| JsError::new(&e))?;
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_values_print_without_floats() {
        let f = |m: i128, s: u8| {
            value_text(&OnyxValue::Fixed {
                mantissa: m,
                scale: s,
            })
        };
        assert_eq!(f(12345, 2), "123.45");
        assert_eq!(f(5, 3), "0.005");
        assert_eq!(f(-5, 3), "-0.005");
        assert_eq!(f(-12345, 2), "-123.45");
        assert_eq!(f(0, 1), "0.0");
        assert_eq!(value_text(&OnyxValue::UInt(7)), "7");
    }

    #[test]
    fn quantities_are_strict() {
        assert_eq!(quantity(&json!("0x7a69")), Some(31_337));
        assert_eq!(quantity(&json!("0x0")), Some(0));
        for bad in [
            json!("0x"),
            json!("7a69"),
            json!("0x07"),
            json!(5),
            json!("0xzz"),
        ] {
            assert_eq!(quantity(&bad), None, "{bad}");
        }
    }
}
