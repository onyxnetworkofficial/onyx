//! Live futures markets in the browser (NORTH-STAR NS11, ADR-0042 amendment
//! 2): the Markets page reads `OnyxFutures` and `OnyxObservedFacts` on Base
//! Sepolia through the reader's own RPC (a public one by default, no API key)
//! and shows what the chain holds. The page only carries the reads: this
//! module names the calls ([`markets_calls`]) and decodes and checks the
//! answers ([`markets_live`]).
//!
//! The site's file (`static/markets/base-sepolia.json`) holds only what the
//! chain cannot say: the contract addresses and each market's title, record
//! date and scale. Everything else is read from the chain. Refused, per
//! market, unless the RPC is on the file's chain, the market exists, its fact
//! is the file's (so a title cannot be put on another market), its edges
//! ascend, its pools are one per bucket and add up to its total, and a
//! resolved market's winner is the bucket of the value `OnyxObservedFacts`
//! holds for its fact at level 2 or better.
//!
//! What it does not prove: that the RPC tells the truth (one RPC's answer at
//! its latest block, as the page says), or that the registry's recorded result
//! is what the observers' reveals aggregate to (the coordinator's attestation,
//! THREATS NL6-2; the daily values' own check does that off chain).
//!
//! Test insurance (north-star loop NL18, ADR-0042 amendment 3) is read the same
//! way when the file names it: `OnyxParametric`'s products, its pool and the
//! payouts it holds back. Refused, per product, unless it exists, its fact is
//! the file's, its times are in order, it sold no more than its cap, and a
//! settled product's outcome is what the guarded facts' value says (paid when
//! the value meets the threshold, expired when not); refused as a whole when
//! the held-back payouts exceed the pool.
//!
//! Correction markets (contributor loop CL7, ADR-0057) are read the same way
//! when the file names them: `OnyxCorrections.market(id)`, its market count and
//! its fee, at the page's one block, which the page passes with the reads.
//! Refused, per market, unless it exists, its 25 words are well formed, its
//! fact and metric are the file's, its close comes before its deadline, its
//! two pools add up to its total and match its bettor counts, and its outcome
//! is what its own numbers say: Yes ("accurate") when the recheck value is
//! within the tolerance of the value, No otherwise; a resolved market's payout
//! pool is the losing side less the fee; a void market names its reason; a
//! market settled from a recheck is read at or after its close block; and the
//! payout fields hold only what the contract can write in that state (none
//! while open, never more left to pay than was owed). The contract counts
//! blocks: the page shows each block with an estimated time from the file's
//! block time and the reader's clock, never decides by it. What it does not
//! prove: that the value and the recheck value are the registry's recorded
//! results (the contract checked both against their result hashes when it
//! opened and resolved; a client reruns that off chain), or that the recheck
//! observers did not bet (the coordinator attests it, ADR-0057 item 5).
//!
//! tONYX is a test token with no value; nothing here moves money.

use onyx_core::keccak256;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// tONYX decimals.
pub const TOKEN_DECIMALS: u32 = 18;
/// Parts per million.
const PPM: u128 = 1_000_000;
/// The futures' `State` values.
const STATES: [&str; 4] = ["none", "open", "resolved", "void"];
/// `OnyxParametric`'s product `State` values.
const PRODUCT_STATES: [&str; 5] = ["none", "open", "paid", "expired", "void"];
/// The largest proof level that settles a policy (`OnyxParametric.MAX_LEVEL`).
const MAX_POLICY_LEVEL: u8 = 2;
/// `OnyxCorrections` sides: Yes ("accurate") and No.
const YES: u8 = 1;
const NO: u8 = 2;
/// `OnyxCorrections.MAX_FEE_PPM`.
const MAX_FEE_PPM: u128 = 100_000;
/// `OnyxCorrections.Market` is a static struct: 25 words in place.
const CORRECTION_WORDS: usize = 25;
/// The slowest block time the file may name, in milliseconds.
const MAX_BLOCK_MS: u64 = 60_000;
/// The last second `Date` and ISO 8601 can show (9999-12-31T23:59:59Z).
const MAX_SHOWN_TIME: i128 = 253_402_300_799;

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes()).0;
    [h[0], h[1], h[2], h[3]]
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn word_u64(n: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&n.to_be_bytes());
    w
}

fn calldata(sig: &str, words: &[[u8; 32]]) -> String {
    let mut d = selector(sig).to_vec();
    for w in words {
        d.extend_from_slice(w);
    }
    hex0x(&d)
}

fn parse_b32(s: &str) -> Result<[u8; 32], String> {
    let v = hex::decode(s.trim_start_matches("0x")).map_err(|e| format!("{s}: {e}"))?;
    v.try_into().map_err(|_| format!("{s}: not 32 bytes"))
}

fn parse_addr(s: &str) -> Result<String, String> {
    let v = hex::decode(s.trim_start_matches("0x")).map_err(|e| format!("{s}: {e}"))?;
    if v.len() != 20 {
        return Err(format!("{s}: not an address"));
    }
    Ok(hex0x(&v))
}

/// The site file's fields this module uses.
struct Config {
    chain_id: u64,
    futures: String,
    facts: String,
    markets: Vec<(u64, [u8; 32], u8)>,
    insurance: Option<Insurance>,
    corrections: Option<Corrections>,
}

/// The file's `corrections` (CL7, ADR-0057): the `OnyxCorrections` contract,
/// the chain's block time (to show each block as a time, about) and each
/// market's id, fact, metric key and scale.
struct Corrections {
    address: String,
    block_ms: u64,
    markets: Vec<CorrectionEntry>,
}

struct CorrectionEntry {
    id: u64,
    fact: [u8; 32],
    /// `keccak256(metric id)`, the registry's metric key (`onyx_core::onchain::metric_key`).
    metric: [u8; 32],
    scale: u8,
}

fn corrections_config(c: &Value) -> Result<Corrections, String> {
    let address = parse_addr(
        c["address"]
            .as_str()
            .ok_or("markets file: corrections without address")?,
    )?;
    let block_ms = c["blockMs"]
        .as_u64()
        .filter(|ms| (1..=MAX_BLOCK_MS).contains(ms))
        .ok_or("markets file: corrections without a block time (blockMs, 1 to 60000)")?;
    let list = c["markets"]
        .as_array()
        .ok_or("markets file: corrections without markets")?;
    let mut markets = vec![];
    for (m, (id, fact, scale)) in list
        .iter()
        .zip(entries(&c["markets"], "correction market")?)
    {
        let metric = m["metric"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(format!("correction market {id} without metric"))?;
        markets.push(CorrectionEntry {
            id,
            fact,
            metric: keccak256(metric.as_bytes()).0,
            scale,
        });
    }
    Ok(Corrections {
        address,
        block_ms,
        markets,
    })
}

/// The file's `insurance`: the policy contract, the (guarded) facts it
/// settles from, and each product's id, fact and scale; with its `markets`,
/// the guarded futures and the markets open there (north-star loop NL23).
struct Insurance {
    address: String,
    facts: String,
    products: Vec<(u64, [u8; 32], u8)>,
    futures: Option<String>,
    markets: Vec<(u64, [u8; 32], u8)>,
}

fn entries(list: &Value, what: &str) -> Result<Vec<(u64, [u8; 32], u8)>, String> {
    let mut out = vec![];
    for m in list.as_array().ok_or(format!("markets file: no {what}s"))? {
        let id = m["id"].as_u64().ok_or(format!("{what} without id"))?;
        let fact = parse_b32(m["fact"].as_str().ok_or(format!("{what} without fact"))?)?;
        let scale = u8::try_from(m["scale"].as_u64().ok_or(format!("{what} without scale"))?)
            .map_err(|e| e.to_string())?;
        if scale > 18 {
            return Err(format!("{what} {id}: scale {scale}"));
        }
        out.push((id, fact, scale));
    }
    Ok(out)
}

fn config(text: &str) -> Result<Config, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("markets file: {e}"))?;
    let s = |k: &str| v[k].as_str().ok_or(format!("markets file: no {k}"));
    let markets = entries(&v["markets"], "market")?;
    let i = &v["insurance"];
    let insurance = if i.is_null() {
        None
    } else {
        let a = |k: &str| {
            i[k].as_str()
                .ok_or(format!("markets file: insurance without {k}"))
        };
        let markets = if i["markets"].is_null() {
            vec![]
        } else {
            entries(&i["markets"], "guarded market")?
        };
        let futures = if markets.is_empty() {
            None
        } else {
            Some(parse_addr(a("futures")?)?)
        };
        Some(Insurance {
            address: parse_addr(a("address")?)?,
            facts: parse_addr(a("facts")?)?,
            products: entries(&i["products"], "product")?,
            futures,
            markets,
        })
    };
    let corrections = if v["corrections"].is_null() {
        None
    } else {
        Some(corrections_config(&v["corrections"])?)
    };
    Ok(Config {
        chain_id: v["chainId"].as_u64().ok_or("markets file: no chainId")?,
        futures: parse_addr(s("futures")?)?,
        facts: parse_addr(s("facts")?)?,
        markets,
        insurance,
        corrections,
    })
}

/// The reads the page sends to its RPC (`eth_call` at its latest block):
/// `marketCount()`, and per market `market(id)` and `factValue(fact)`; with
/// `corrections`, `OnyxCorrections`' `marketCount()`, `feePpm()` and per
/// market `market(id)`.
#[wasm_bindgen]
pub fn markets_calls(config_json: &str) -> Result<String, JsError> {
    calls(config_json).map_err(|e| JsError::new(&e))
}

fn calls(config_json: &str) -> Result<String, String> {
    let c = config(config_json)?;
    let mut out =
        vec![json!({"key": "count", "to": c.futures, "data": calldata("marketCount()", &[])})];
    for (id, fact, _) in &c.markets {
        out.push(json!({"key": format!("market:{id}"), "to": c.futures, "data": calldata("market(uint256)", &[word_u64(*id)])}));
        out.push(json!({"key": format!("fact:{id}"), "to": c.facts, "data": calldata("factValue(bytes32)", &[*fact])}));
    }
    if let Some(i) = &c.insurance {
        for (key, sig) in [
            ("ins:count", "productCount()"),
            ("ins:pool", "pool()"),
            ("ins:reserved", "reserved()"),
        ] {
            out.push(json!({"key": key, "to": i.address, "data": calldata(sig, &[])}));
        }
        for (id, fact, _) in &i.products {
            out.push(json!({"key": format!("product:{id}"), "to": i.address, "data": calldata("product(uint256)", &[word_u64(*id)])}));
            out.push(json!({"key": format!("pfact:{id}"), "to": i.facts, "data": calldata("factValue(bytes32)", &[*fact])}));
        }
        if let Some(f) = &i.futures {
            out.push(json!({"key": "gcount", "to": f, "data": calldata("marketCount()", &[])}));
            for (id, fact, _) in &i.markets {
                out.push(json!({"key": format!("gmarket:{id}"), "to": f, "data": calldata("market(uint256)", &[word_u64(*id)])}));
                out.push(json!({"key": format!("gfact:{id}"), "to": i.facts, "data": calldata("factValue(bytes32)", &[*fact])}));
            }
        }
    }
    if let Some(k) = &c.corrections {
        for (key, sig) in [("corr:count", "marketCount()"), ("corr:fee", "feePpm()")] {
            out.push(json!({"key": key, "to": k.address, "data": calldata(sig, &[])}));
        }
        for e in &k.markets {
            out.push(json!({"key": format!("correction:{}", e.id), "to": k.address, "data": calldata("market(uint256)", &[word_u64(e.id)])}));
        }
    }
    Ok(json!({"chainId": c.chain_id, "calls": out}).to_string())
}

/// ABI words of a return value.
struct Words(Vec<[u8; 32]>);

impl Words {
    fn of(hexs: &str) -> Result<Self, String> {
        let b = hex::decode(hexs.trim_start_matches("0x")).map_err(|e| format!("return: {e}"))?;
        if b.is_empty() || b.len() % 32 != 0 {
            return Err(format!("return of {} bytes is no ABI value", b.len()));
        }
        Ok(Words(
            b.chunks(32)
                .map(|c| c.try_into().unwrap_or([0; 32]))
                .collect(),
        ))
    }

    fn word(&self, i: usize) -> Result<&[u8; 32], String> {
        self.0.get(i).ok_or(format!("return too short (word {i})"))
    }

    /// An unsigned word that must fit `bits`.
    fn uint(&self, i: usize, bits: u32) -> Result<u128, String> {
        let w = self.word(i)?;
        if w[..16].iter().any(|&b| b != 0) {
            return Err(format!("word {i} does not fit 128 bits"));
        }
        let n = u128::from_be_bytes(w[16..].try_into().map_err(|_| "word")?);
        if bits < 128 && n >> bits != 0 {
            return Err(format!("word {i} does not fit {bits} bits"));
        }
        Ok(n)
    }

    /// A signed word that must fit i128.
    fn int(&self, i: usize) -> Result<i128, String> {
        let w = self.word(i)?;
        let lo = i128::from_be_bytes(w[16..].try_into().map_err(|_| "word")?);
        let ext = if lo < 0 { 0xff } else { 0 };
        if w[..16].iter().any(|&b| b != ext) {
            return Err(format!("word {i} does not fit 128 bits"));
        }
        Ok(lo)
    }

    /// The array at byte offset `off` (relative to word `base`).
    fn array(&self, base: usize, off: u128) -> Result<(usize, usize), String> {
        if !off.is_multiple_of(32) {
            return Err("an array offset is not word aligned".into());
        }
        let at = usize::try_from(off / 32)
            .ok()
            .and_then(|o| base.checked_add(o))
            .ok_or("an array offset runs past the return")?;
        let n = usize::try_from(self.uint(at, 32)?).map_err(|_| "length")?;
        match at.checked_add(1).and_then(|s| s.checked_add(n)) {
            Some(end) if end <= self.0.len() => Ok((at + 1, n)),
            _ => Err("an array runs past the return".into()),
        }
    }
}

/// `OnyxFutures.Market`, decoded.
struct Market {
    fact: [u8; 32],
    trading_close: u64,
    observable_at: u64,
    resolve_by: u64,
    state: u8,
    winner: u8,
    bettors: u32,
    total: u128,
    value: i128,
    edges: Vec<i128>,
    pools: Vec<u128>,
    counts: Vec<u32>,
}

fn market(hexs: &str) -> Result<Market, String> {
    let w = Words::of(hexs)?;
    // A struct with dynamic members: one offset word, then the tuple.
    if w.uint(0, 64)? != 32 {
        return Err("the market return does not start with its tuple offset".into());
    }
    let t = 1;
    let u64w = |i: usize| -> Result<u64, String> { Ok(w.uint(t + i, 64)? as u64) };
    let (e0, en) = w.array(t, w.uint(t + 14, 64)?)?;
    let (p0, pn) = w.array(t, w.uint(t + 15, 64)?)?;
    let (c0, cn) = w.array(t, w.uint(t + 16, 64)?)?;
    Ok(Market {
        fact: *w.word(t)?,
        trading_close: u64w(1)?,
        observable_at: u64w(2)?,
        resolve_by: u64w(3)?,
        state: w.uint(t + 4, 8)? as u8,
        winner: w.uint(t + 5, 8)? as u8,
        bettors: w.uint(t + 7, 32)? as u32,
        total: w.uint(t + 9, 128)?,
        value: w.int(t + 13)?,
        edges: (0..en).map(|k| w.int(e0 + k)).collect::<Result<_, _>>()?,
        pools: (0..pn)
            .map(|k| w.uint(p0 + k, 128))
            .collect::<Result<_, _>>()?,
        counts: (0..cn)
            .map(|k| w.uint(c0 + k, 32).map(|n| n as u32))
            .collect::<Result<_, _>>()?,
    })
}

/// `factValue(fact)`: (final, value, level, acceptedAt).
fn fact_value(hexs: &str) -> Result<(bool, i128, u8, u64), String> {
    let w = Words::of(hexs)?;
    Ok((
        w.uint(0, 1)? == 1,
        w.int(1)?,
        w.uint(2, 8)? as u8,
        w.uint(3, 64)? as u64,
    ))
}

/// A mantissa at `scale` as a decimal string.
pub fn decimal(m: i128, scale: u32) -> String {
    let s = decimal_u(m.unsigned_abs(), scale);
    if m < 0 {
        format!("-{s}")
    } else {
        s
    }
}

/// An unsigned mantissa (a `uint128` word) at `scale` as a decimal string,
/// exact up to `u128::MAX`.
pub fn decimal_u(a: u128, scale: u32) -> String {
    let p = 10u128.pow(scale);
    let (i, f) = (a / p, a % p);
    if scale == 0 {
        i.to_string()
    } else {
        format!("{i}.{f:0width$}", width = scale as usize)
    }
}

/// The bucket `value` falls in: the number of edges at or below it
/// (`OnyxFutures.bucketOf`).
pub fn bucket_of(edges: &[i128], value: i128) -> usize {
    edges.iter().filter(|e| **e <= value).count()
}

/// Decode and check every market of the file against the page's reads
/// (`{"chainId": "0x...", "block": "0x...", "results": {key: return hex}}`,
/// `block` the one block every read was sent at) at the reader's clock `now`.
/// Every figure the page shows comes from here.
#[wasm_bindgen]
pub fn markets_live(config_json: &str, reads_json: &str, now: u64) -> Result<String, JsError> {
    live(config_json, reads_json, now).map_err(|e| JsError::new(&e))
}

fn live(config_json: &str, reads_json: &str, now: u64) -> Result<String, String> {
    let c = config(config_json)?;
    let r: Value = serde_json::from_str(reads_json).map_err(|e| format!("reads: {e}"))?;
    let chain = r["chainId"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    if chain != Some(c.chain_id) {
        return Ok(json!({"ok": false, "problems": [format!("the RPC is not on chain {} (Base Sepolia)", c.chain_id)], "markets": []}).to_string());
    }
    let get = |k: &str| -> Result<String, String> {
        r["results"][k]
            .as_str()
            .map(str::to_owned)
            .ok_or(format!("no read {k}"))
    };
    let count = Words::of(&get("count")?)?.uint(0, 64)? as u64;
    let mut out = vec![];
    let mut all_ok = true;
    for (id, fact, scale) in &c.markets {
        let v = one("", *id, fact, u32::from(*scale), count, &get, now);
        all_ok &= v["ok"] == json!(true);
        out.push(v);
    }
    let insurance = match &c.insurance {
        None => Value::Null,
        Some(i) => {
            let v = insurance(i, &get, now);
            all_ok &= v["ok"] == json!(true);
            v
        }
    };
    let corrections = match &c.corrections {
        None => Value::Null,
        Some(k) => {
            let v = corrections(k, read_block(&r["block"]), &get, now);
            all_ok &= v["ok"] == json!(true);
            v
        }
    };
    Ok(json!({"ok": all_ok, "problems": [], "count": count, "markets": out, "insurance": insurance, "corrections": corrections}).to_string())
}

/// The page's block (`0x` and 1 to 16 hex digits), or `None`.
fn read_block(v: &Value) -> Option<u64> {
    let h = v.as_str()?.strip_prefix("0x")?;
    if h.is_empty() || h.len() > 16 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(h, 16).ok()
}

/// `OnyxCorrections.Market`, decoded (the fields the page shows or checks).
struct Correction {
    metric: [u8; 32],
    fact: [u8; 32],
    close_block: u64,
    resolve_block: u64,
    state: u8,
    winner: u8,
    void_reason: u8,
    bettors: u32,
    /// Winners (resolved) or bettors (void) not paid yet.
    winners_left: u32,
    value: i128,
    recheck_value: i128,
    tolerance: u128,
    total: u128,
    winning_pool: u128,
    distributable: u128,
    /// What the market still owes its winners or bettors.
    left: u128,
    /// Yes, No.
    pools: [u128; 2],
    counts: [u32; 2],
}

fn correction(hexs: &str) -> Result<Correction, String> {
    let w = Words::of(hexs)?;
    // `uint128[2]` and `uint32[2]` are fixed arrays: no offset word, no tail.
    if w.0.len() != CORRECTION_WORDS {
        return Err(format!(
            "a correction market is {CORRECTION_WORDS} words, not {}",
            w.0.len()
        ));
    }
    // Word 4 is the opener, an address: twelve zero bytes first.
    if w.word(4)?[..12].iter().any(|&b| b != 0) {
        return Err("word 4 is not an address".into());
    }
    // Not used, but a malformed word refuses the whole answer: windowEnd and openFee.
    for (i, bits) in [(5, 64), (16, 128)] {
        w.uint(i, bits)?;
    }
    Ok(Correction {
        metric: *w.word(0)?,
        fact: *w.word(2)?,
        close_block: w.uint(6, 64)? as u64,
        resolve_block: w.uint(7, 64)? as u64,
        state: w.uint(8, 8)? as u8,
        winner: w.uint(9, 8)? as u8,
        void_reason: w.uint(10, 8)? as u8,
        bettors: w.uint(11, 32)? as u32,
        winners_left: w.uint(12, 32)? as u32,
        value: w.int(13)?,
        recheck_value: w.int(14)?,
        tolerance: w.uint(15, 128)?,
        total: w.uint(17, 128)?,
        winning_pool: w.uint(18, 128)?,
        distributable: w.uint(19, 128)?,
        left: w.uint(20, 128)?,
        pools: [w.uint(21, 128)?, w.uint(22, 128)?],
        counts: [w.uint(23, 32)? as u32, w.uint(24, 32)? as u32],
    })
}

/// The side a recheck value wins for (`OnyxCorrections.resolve`): Yes when it
/// is within `tolerance` of the value, else No.
pub fn correction_side(value: i128, recheck: i128, tolerance: u128) -> u8 {
    if recheck.abs_diff(value) <= tolerance {
        YES
    } else {
        NO
    }
}

/// `OnyxPayout.fee`, `floor(losers * feePpm / 1e6)`, without overflow.
fn payout_fee(losers: u128, fee_ppm: u128) -> u128 {
    (losers / PPM) * fee_ppm + (losers % PPM) * fee_ppm / PPM
}

/// The time block `b` is expected at, from the page's block `head` at the
/// reader's clock `now` and the file's block time; `None` past what a page can
/// show. An estimate: the contract counts blocks, never seconds.
pub fn block_time(b: u64, head: u64, block_ms: u64, now: u64) -> Option<u64> {
    let t = i128::from(now) + (i128::from(b) - i128::from(head)) * i128::from(block_ms) / 1000;
    if (0..=MAX_SHOWN_TIME).contains(&t) {
        u64::try_from(t).ok()
    } else {
        None
    }
}

fn corrections(
    k: &Corrections,
    head: Option<u64>,
    get: &dyn Fn(&str) -> Result<String, String>,
    now: u64,
) -> Value {
    let refuse = |why: String| json!({"ok": false, "problems": [format!("correction markets: {why}")], "markets": []});
    let Some(head) = head else {
        return refuse("no block number with the reads".into());
    };
    let read = (|| -> Result<(u64, u128), String> {
        Ok((
            Words::of(&get("corr:count")?)?.uint(0, 64)? as u64,
            Words::of(&get("corr:fee")?)?.uint(0, 32)?,
        ))
    })();
    let (count, fee_ppm) = match read {
        Ok(h) => h,
        Err(e) => return refuse(e),
    };
    if fee_ppm > MAX_FEE_PPM {
        return refuse(format!("a fee of {fee_ppm} ppm, over the contract's 10%"));
    }
    let markets: Vec<Value> = k
        .markets
        .iter()
        .map(|e| one_correction(e, count, fee_ppm, head, k.block_ms, get, now))
        .collect();
    json!({
        "ok": markets.iter().all(|m| m["ok"] == json!(true)),
        "problems": [],
        "count": count,
        "feePpm": fee_ppm,
        "block": head,
        "markets": markets,
    })
}

/// What a market's outcome fields must hold in `state` at the page's block
/// `head`, as `OnyxCorrections` writes them: `open` sets none of them;
/// `resolve` (only from the close block on) sets the recheck value and the
/// winner, then either voids (reason 1, the winning side had no bet) or sets
/// the winners' pool, their payout (the losing side less the fee), `left` and
/// `winnersLeft`; `void` (reason 2, no valid recheck, also before the close)
/// sets only `left` and `winnersLeft`; each withdrawal lowers both.
fn outcome_problems(m: &Correction, state: &str, fee_ppm: u128, head: u64) -> Vec<String> {
    let mut problems: Vec<String> = vec![];
    let side = correction_side(m.value, m.recheck_value, m.tolerance);
    let pool_of = |s: u8| m.pools[usize::from(s - 1)];
    let rechecked = state == "resolved" || (state == "void" && m.void_reason == 1);
    if rechecked && head < m.close_block {
        problems.push("settled from a recheck before its close block".into());
    }
    match state {
        "open" => {
            let payout = m.winning_pool | m.distributable | m.left;
            if m.winner != 0
                || m.void_reason != 0
                || m.recheck_value != 0
                || payout != 0
                || m.winners_left != 0
            {
                problems
                    .push("an open market already holds a recheck, a winner or a payout".into());
            }
        }
        "resolved" => {
            let losers = m.total.saturating_sub(pool_of(side));
            if m.winner != side || m.void_reason != 0 {
                problems
                    .push("the winner is not what the recheck value and the tolerance say".into());
            } else if pool_of(side) == 0
                || m.winning_pool != pool_of(side)
                || m.distributable != losers - payout_fee(losers, fee_ppm)
            {
                problems.push("the winners' pool or payout is not the pools less the fee".into());
            } else {
                if m.winners_left > m.counts[usize::from(side - 1)] {
                    problems.push("more winners left to pay than bet on the winning side".into());
                }
                if m.winning_pool
                    .checked_add(m.distributable)
                    .is_none_or(|owed| m.left > owed)
                {
                    problems.push("more left to pay than the winners' pool and payout".into());
                }
            }
        }
        "void" => {
            match m.void_reason {
                1 if m.winner != side || pool_of(side) != 0 => {
                    problems
                        .push("void because the winning side had no bet, but it has bets".into());
                }
                2 if m.winner != 0 || m.recheck_value != 0 => {
                    problems.push(
                        "void for no valid recheck, but it names a winner or a recheck value"
                            .into(),
                    );
                }
                1 | 2 => {}
                r => problems.push(format!("void reason {r}")),
            }
            if m.winners_left > m.bettors || m.left > m.total {
                problems.push("more left to refund than was bet".into());
            }
        }
        _ => {}
    }
    problems
}

fn one_correction(
    e: &CorrectionEntry,
    count: u64,
    fee_ppm: u128,
    head: u64,
    block_ms: u64,
    get: &dyn Fn(&str) -> Result<String, String>,
    now: u64,
) -> Value {
    let id = e.id;
    let refuse = |why: String| json!({"id": id, "ok": false, "problems": [why]});
    if id == 0 || id > count {
        return refuse(format!(
            "correction market {id} does not exist on chain ({count} markets)"
        ));
    }
    let m = match get(&format!("correction:{id}")).and_then(|h| correction(&h)) {
        Ok(m) => m,
        Err(e) => return refuse(format!("correction market {id}: {e}")),
    };
    let mut problems: Vec<String> = vec![];
    if m.fact != e.fact {
        problems.push("the chain's market names another fact than this page's title".into());
    }
    if m.metric != e.metric {
        problems.push("the chain's market is on another metric than this page's".into());
    }
    let state = STATES
        .get(usize::from(m.state))
        .copied()
        .unwrap_or("unknown");
    if !matches!(state, "open" | "resolved" | "void") {
        problems.push(format!("market state {}", m.state));
    }
    if m.close_block >= m.resolve_block {
        problems.push("the market's close is not before its deadline".into());
    }
    if m.pools[0].checked_add(m.pools[1]) != Some(m.total) {
        problems.push("the two pools do not add up to the market's total".into());
    }
    // A side holds money exactly when someone bet on it; a bettor may bet on both.
    let sides = (0..2).all(|k| (m.pools[k] == 0) == (m.counts[k] == 0));
    let both = u64::from(m.counts[0]) + u64::from(m.counts[1]);
    if !sides || m.bettors < m.counts[0].max(m.counts[1]) || u64::from(m.bettors) > both {
        problems.push("the bettor counts do not match the pools".into());
    }
    problems.extend(outcome_problems(&m, state, fee_ppm, head));
    // An RPC's answer is untrusted: no overflow, no division by zero.
    let share = |p: u128| -> u128 { p.saturating_mul(PPM).checked_div(m.total).unwrap_or(0) };
    let side_json = |k: usize| json!({"pool": tokens(m.pools[k]), "bettors": m.counts[k], "sharePpm": share(m.pools[k]).to_string()});
    // A recheck value exists once the market resolved, or voided for no winning bet.
    let rechecked = state == "resolved" || (state == "void" && m.void_reason == 1);
    let at = |b: u64| block_time(b, head, block_ms, now).map_or(Value::Null, |t| json!(t));
    json!({
        "id": id,
        "ok": problems.is_empty(),
        "problems": problems,
        "state": state,
        "betting": state == "open" && head < m.close_block,
        "pastDeadline": state == "open" && head > m.resolve_block,
        "value": decimal(m.value, u32::from(e.scale)),
        "tolerance": decimal_u(m.tolerance, u32::from(e.scale)),
        "yes": side_json(0),
        "no": side_json(1),
        "total": tokens(m.total),
        "bettors": m.bettors,
        "closeBlock": m.close_block,
        "resolveBlock": m.resolve_block,
        "closeAt": at(m.close_block),
        "resolveAt": at(m.resolve_block),
        "recheckValue": if rechecked { json!(decimal(m.recheck_value, u32::from(e.scale))) } else { Value::Null },
        "winner": match (state, m.winner) {
            ("resolved", YES) => json!("yes"),
            ("resolved", NO) => json!("no"),
            _ => Value::Null,
        },
        "voidReason": match (state, m.void_reason) {
            ("void", 1) => json!("nobet"),
            ("void", 2) => json!("norecheck"),
            _ => Value::Null,
        },
    })
}

/// `OnyxParametric.Product`, decoded (a static struct: twelve words).
struct Product {
    fact: [u8; 32],
    cmp: u8,
    state: u8,
    sold: u32,
    max_policies: u32,
    buy_by: u64,
    observable_at: u64,
    resolve_by: u64,
    threshold: i128,
    payout: u128,
    premium: u128,
    value: i128,
}

fn product(hexs: &str) -> Result<Product, String> {
    let w = Words::of(hexs)?;
    if w.0.len() != 12 {
        return Err(format!("a product is 12 words, not {}", w.0.len()));
    }
    Ok(Product {
        fact: *w.word(0)?,
        cmp: w.uint(1, 8)? as u8,
        state: w.uint(2, 8)? as u8,
        sold: w.uint(3, 32)? as u32,
        max_policies: w.uint(4, 32)? as u32,
        buy_by: w.uint(5, 64)? as u64,
        observable_at: w.uint(6, 64)? as u64,
        resolve_by: w.uint(7, 64)? as u64,
        threshold: w.int(8)?,
        payout: w.uint(9, 128)?,
        premium: w.uint(10, 128)?,
        value: w.int(11)?,
    })
}

/// Whether `value` meets a product's threshold (`OnyxParametric.settle`).
pub fn meets(cmp: u8, value: i128, threshold: i128) -> bool {
    if cmp == 0 {
        value >= threshold
    } else {
        value <= threshold
    }
}

fn tokens(n: u128) -> String {
    decimal_u(n, TOKEN_DECIMALS)
}

fn insurance(i: &Insurance, get: &dyn Fn(&str) -> Result<String, String>, now: u64) -> Value {
    let head = (|| -> Result<(u64, u128, u128), String> {
        Ok((
            Words::of(&get("ins:count")?)?.uint(0, 64)? as u64,
            Words::of(&get("ins:pool")?)?.uint(0, 128)?,
            Words::of(&get("ins:reserved")?)?.uint(0, 128)?,
        ))
    })();
    let (count, pool, reserved) = match head {
        Ok(h) => h,
        Err(e) => {
            return json!({"ok": false, "problems": [format!("insurance: {e}")], "products": []})
        }
    };
    let mut problems = vec![];
    if reserved > pool {
        problems.push("the payouts held back exceed the pool".to_string());
    }
    let products: Vec<Value> = i
        .products
        .iter()
        .map(|(id, fact, scale)| one_product(*id, fact, u32::from(*scale), count, get, now))
        .collect();
    // Markets on the guarded futures settle from the same guarded facts.
    let markets: Vec<Value> = if i.markets.is_empty() {
        vec![]
    } else {
        match get("gcount").and_then(|h| Ok(Words::of(&h)?.uint(0, 64)? as u64)) {
            Ok(n) => i
                .markets
                .iter()
                .map(|(id, fact, scale)| one("g", *id, fact, u32::from(*scale), n, get, now))
                .collect(),
            Err(e) => {
                problems.push(format!("guarded markets: {e}"));
                vec![]
            }
        }
    };
    let ok = problems.is_empty()
        && products.iter().all(|p| p["ok"] == json!(true))
        && markets.iter().all(|m| m["ok"] == json!(true));
    json!({
        "ok": ok,
        "problems": problems,
        "count": count,
        "pool": tokens(pool),
        "reserved": tokens(reserved),
        "free": tokens(pool.saturating_sub(reserved)),
        "products": products,
        "markets": markets,
    })
}

fn one_product(
    id: u64,
    fact: &[u8; 32],
    scale: u32,
    count: u64,
    get: &dyn Fn(&str) -> Result<String, String>,
    now: u64,
) -> Value {
    let refuse = |why: String| json!({"id": id, "ok": false, "problems": [why]});
    if id == 0 || id > count {
        return refuse(format!(
            "product {id} does not exist on chain ({count} products)"
        ));
    }
    let p = match get(&format!("product:{id}")).and_then(|h| product(&h)) {
        Ok(p) => p,
        Err(e) => return refuse(format!("product {id}: {e}")),
    };
    let (fin, value, level, _) = match get(&format!("pfact:{id}")).and_then(|h| fact_value(&h)) {
        Ok(f) => f,
        Err(e) => return refuse(format!("fact of product {id}: {e}")),
    };
    let mut problems = vec![];
    if p.fact != *fact {
        problems.push("the chain's product names another fact than this page's title".to_string());
    }
    if p.cmp > 1 {
        problems.push(format!("comparator {}", p.cmp));
    }
    let state = PRODUCT_STATES
        .get(usize::from(p.state))
        .copied()
        .unwrap_or("unknown");
    if !matches!(state, "open" | "paid" | "expired" | "void") {
        problems.push(format!("product state {}", p.state));
    }
    if p.sold > p.max_policies {
        problems.push("more policies sold than the product's cap".into());
    }
    if !(p.buy_by < p.observable_at && p.observable_at < p.resolve_by) {
        problems.push("the product's times are out of order".into());
    }
    if matches!(state, "paid" | "expired") {
        if !fin || level == 0 || level > MAX_POLICY_LEVEL || value != p.value {
            problems.push("settled, but the facts hold no matching level-2 value".into());
        } else if meets(p.cmp, value, p.threshold) != (state == "paid") {
            problems.push("the outcome is not what the value and the threshold say".into());
        }
    }
    json!({
        "id": id,
        "ok": problems.is_empty(),
        "problems": problems,
        "state": state,
        "buying": state == "open" && now < p.buy_by,
        "cmp": if p.cmp == 0 { "atleast" } else { "atmost" },
        "threshold": decimal(p.threshold, scale),
        "payout": tokens(p.payout),
        "premium": tokens(p.premium),
        "sold": p.sold,
        "maxPolicies": p.max_policies,
        "buyBy": p.buy_by,
        "observableAt": p.observable_at,
        "resolveBy": p.resolve_by,
        "value": if fin { json!(decimal(value, scale)) } else { Value::Null },
        "level": if fin { json!(level) } else { Value::Null },
    })
}

/// One market; `pre` names its reads (`""` the first futures, `"g"` the
/// guarded futures).
fn one(
    pre: &str,
    id: u64,
    fact: &[u8; 32],
    scale: u32,
    count: u64,
    get: &dyn Fn(&str) -> Result<String, String>,
    now: u64,
) -> Value {
    let refuse = |why: String| json!({"id": id, "ok": false, "problems": [why]});
    if id == 0 || id > count {
        return refuse(format!(
            "market {id} does not exist on chain ({count} markets)"
        ));
    }
    let m = match get(&format!("{pre}market:{id}")).and_then(|h| market(&h)) {
        Ok(m) => m,
        Err(e) => return refuse(format!("market {id}: {e}")),
    };
    let fv = match get(&format!("{pre}fact:{id}")).and_then(|h| fact_value(&h)) {
        Ok(f) => f,
        Err(e) => return refuse(format!("fact of market {id}: {e}")),
    };
    let mut problems = vec![];
    if m.fact != *fact {
        problems.push("the chain's market names another fact than this page's title".to_string());
    }
    if m.edges.is_empty() || m.edges.windows(2).any(|w| w[0] >= w[1]) {
        problems.push("the bucket edges do not ascend".into());
    }
    if m.pools.len() != m.edges.len() + 1 || m.counts.len() != m.pools.len() {
        problems.push("the pools are not one per bucket".into());
    }
    if m.pools.iter().try_fold(0u128, |a, p| a.checked_add(*p)) != Some(m.total) {
        problems.push("the pools do not add up to the market's total".into());
    }
    let state = STATES
        .get(usize::from(m.state))
        .copied()
        .unwrap_or("unknown");
    if !matches!(state, "open" | "resolved" | "void") {
        problems.push(format!("market state {}", m.state));
    }
    let (fin, value, level, accepted) = fv;
    if state == "resolved" {
        if !fin || level == 0 || level > 2 {
            problems.push("resolved, but the fact holds no level-2 value".into());
        } else if value != m.value || bucket_of(&m.edges, value) != usize::from(m.winner) {
            problems.push("the winning bucket is not the bucket of the fact's value".into());
        }
    }
    // An RPC's answer is untrusted: no overflow, no division by zero.
    let chance = |p: u128| -> u128 { p.saturating_mul(PPM).checked_div(m.total).unwrap_or(0) };
    let buckets: Vec<Value> = (0..m.pools.len())
        .map(|k| {
            json!({
                "low": if k == 0 { Value::Null } else { json!(decimal(m.edges[k - 1], scale)) },
                "high": m.edges.get(k).map(|e| json!(decimal(*e, scale))).unwrap_or(Value::Null),
                "pool": tokens(m.pools[k]),
                "chancePpm": chance(m.pools[k]).to_string(),
                "bettors": m.counts.get(k).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({
        "id": id,
        "ok": problems.is_empty(),
        "problems": problems,
        "state": state,
        "trading": state == "open" && now < m.trading_close,
        "tradingClose": m.trading_close,
        "observableAt": m.observable_at,
        "resolveBy": m.resolve_by,
        "buckets": buckets,
        "total": tokens(m.total),
        "bettors": m.bettors,
        "value": if fin { json!(decimal(value, scale)) } else { Value::Null },
        "level": if fin { json!(level) } else { Value::Null },
        "acceptedAt": if fin { json!(accepted) } else { Value::Null },
        "winner": if state == "resolved" { json!(m.winner) } else { Value::Null },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FACT: &str = "0x1598f049997f46cda70ff579a1cd92a426ba7dac420709fe9b86b9fe8f7c8975";

    fn config_json() -> String {
        json!({
            "chainId": 84532,
            "futures": "0xd0252e271cc218fabe591895f23a3e6e9e1180ff",
            "facts": "0xe13b89565d037c6ea91d2bc040de78829eac473c",
            "markets": [{"id": 1, "fact": FACT, "scale": 2}],
        })
        .to_string()
    }

    fn w(n: i128) -> String {
        let mut b = [if n < 0 { 0xff } else { 0 }; 32];
        b[16..].copy_from_slice(&n.to_be_bytes());
        hex::encode(b)
    }

    /// `abi.encode(Market)` as `OnyxFutures.market` returns it.
    fn encode(
        fact: &str,
        state: u8,
        winner: u8,
        value: i128,
        edges: &[i128],
        pools: &[u128],
    ) -> String {
        let total: u128 = pools.iter().sum();
        let mut head = vec![
            fact.trim_start_matches("0x").to_string(),
            w(1_793_188_800),
            w(1_793_232_000),
            w(1_793_836_800),
            w(i128::from(state)),
            w(i128::from(winner)),
            w(0),
            w(i128::try_from(pools.len()).unwrap()),
            w(0),
            w(i128::try_from(total).unwrap()),
            w(0),
            w(0),
            w(0),
            w(value),
        ];
        let e_off = 17 * 32;
        let p_off = e_off + (1 + edges.len()) * 32;
        let c_off = p_off + (1 + pools.len()) * 32;
        for o in [e_off, p_off, c_off] {
            head.push(w(i128::try_from(o).unwrap()));
        }
        let mut tail = vec![w(i128::try_from(edges.len()).unwrap())];
        tail.extend(edges.iter().map(|e| w(*e)));
        tail.push(w(i128::try_from(pools.len()).unwrap()));
        tail.extend(pools.iter().map(|p| w(i128::try_from(*p).unwrap())));
        tail.push(w(i128::try_from(pools.len()).unwrap()));
        tail.extend(pools.iter().map(|_| w(1)));
        format!("0x{}{}{}", w(32), head.concat(), tail.concat())
    }

    fn fv(fin: bool, value: i128) -> String {
        format!(
            "0x{}{}{}{}",
            w(i128::from(fin)),
            w(value),
            w(if fin { 2 } else { 0 }),
            w(if fin { 1_793_240_000 } else { 0 })
        )
    }

    fn reads(market: &str, fact: &str) -> String {
        json!({"chainId": "0x14a34", "results": {"count": format!("0x{}", w(1)), "market:1": market, "fact:1": fact}}).to_string()
    }

    const EDGES: [i128; 2] = [4_045_000_000_000_000, 4_050_000_000_000_000];
    const T: u128 = 1_000_000_000_000_000_000;

    #[test]
    fn calls_name_the_reads() {
        let v: Value = serde_json::from_str(&calls(&config_json()).unwrap()).unwrap();
        assert_eq!(v["calls"][0]["data"], "0xec979082");
        assert_eq!(
            v["calls"][1]["to"],
            "0xd0252e271cc218fabe591895f23a3e6e9e1180ff"
        );
        assert!(v["calls"][2]["data"]
            .as_str()
            .unwrap()
            .ends_with(FACT.trim_start_matches("0x")));
    }

    /// The live market 1, as Base Sepolia answered on 2026-10-27 (four bets, open).
    #[test]
    fn an_open_market_shows_its_pools_and_chances() {
        let m = encode(FACT, 1, 0, 0, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let v: Value = serde_json::from_str(
            &live(&config_json(), &reads(&m, &fv(false, 0)), 1_793_130_000).unwrap(),
        )
        .unwrap();
        let x = &v["markets"][0];
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(x["state"], "open");
        assert_eq!(x["trading"], true);
        assert_eq!(x["total"], "125.000000000000000000");
        assert_eq!(x["buckets"][1]["chancePpm"], "600000");
        assert_eq!(x["buckets"][0]["high"], "40450000000000.00");
        assert_eq!(x["buckets"][2]["low"], "40500000000000.00");
        assert_eq!(x["value"], Value::Null);
    }

    /// The bytes Base Sepolia returned for `market(1)` on 2026-10-27 (four bets, open).
    #[test]
    fn the_live_market_1_return_decodes() {
        let m = concat!(
            "0x",
            "0000000000000000000000000000000000000000000000000000000000000020",
            "1598f049997f46cda70ff579a1cd92a426ba7dac420709fe9b86b9fe8f7c8975",
            "000000000000000000000000000000000000000000000000000000006ae1e3c0",
            "000000000000000000000000000000000000000000000000000000006ae28c80",
            "000000000000000000000000000000000000000000000000000000006aebc700",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000004",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000000006c6b935b8bbd40000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000220",
            "0000000000000000000000000000000000000000000000000000000000000280",
            "0000000000000000000000000000000000000000000000000000000000000300",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "000000000000000000000000000000000000000000000000000e5ee7f41dd000",
            "000000000000000000000000000000000000000000000000000e63741b572000",
            "0000000000000000000000000000000000000000000000000000000000000003",
            "000000000000000000000000000000000000000000000001a055690d9db80000",
            "00000000000000000000000000000000000000000000000410d586a20a4c0000",
            "000000000000000000000000000000000000000000000001158e460913d00000",
            "0000000000000000000000000000000000000000000000000000000000000003",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000002",
            "0000000000000000000000000000000000000000000000000000000000000001",
        );
        let v: Value = serde_json::from_str(
            &live(&config_json(), &reads(m, &fv(false, 0)), 1_793_130_000).unwrap(),
        )
        .unwrap();
        let x = &v["markets"][0];
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(x["bettors"], 4);
        assert_eq!(x["total"], "125.000000000000000000");
        assert_eq!(x["buckets"][0]["pool"], "30.000000000000000000");
        assert_eq!(x["buckets"][1]["pool"], "75.000000000000000000");
        assert_eq!(x["buckets"][1]["bettors"], 2);
        assert_eq!(x["tradingClose"], 1_793_188_800);
    }

    #[test]
    fn a_resolved_market_must_pay_the_bucket_of_the_facts_value() {
        let value = 4_046_123_456_789_012;
        let ok = encode(FACT, 2, 1, value, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let v: Value = serde_json::from_str(
            &live(&config_json(), &reads(&ok, &fv(true, value)), 1_793_300_000).unwrap(),
        )
        .unwrap();
        assert_eq!(v["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["markets"][0]["value"], "40461234567890.12");
        let wrong = encode(FACT, 2, 2, value, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let v: Value = serde_json::from_str(
            &live(
                &config_json(),
                &reads(&wrong, &fv(true, value)),
                1_793_300_000,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["markets"][0]["ok"], false);
        let unproved = encode(FACT, 2, 1, value, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let v: Value = serde_json::from_str(
            &live(
                &config_json(),
                &reads(&unproved, &fv(false, 0)),
                1_793_300_000,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["markets"][0]["ok"], false);
    }

    #[test]
    fn another_fact_another_chain_or_bad_books_are_refused() {
        let other = format!("0x{}", "22".repeat(32));
        let m = encode(&other, 1, 0, 0, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let v: Value =
            serde_json::from_str(&live(&config_json(), &reads(&m, &fv(false, 0)), 0).unwrap())
                .unwrap();
        assert_eq!(v["markets"][0]["ok"], false, "a title on another market");
        let mut r: Value = serde_json::from_str(&reads(&m, &fv(false, 0))).unwrap();
        r["chainId"] = json!("0x2105");
        let v: Value =
            serde_json::from_str(&live(&config_json(), &r.to_string(), 0).unwrap()).unwrap();
        assert_eq!(v["ok"], false, "Base mainnet");
        // Pools that do not add up to the total.
        let bad = encode(FACT, 1, 0, 0, &EDGES, &[30 * T, 75 * T, 20 * T]).replacen(
            &w(i128::try_from(125 * T).unwrap()),
            &w(i128::try_from(126 * T).unwrap()),
            1,
        );
        let v: Value =
            serde_json::from_str(&live(&config_json(), &reads(&bad, &fv(false, 0)), 0).unwrap())
                .unwrap();
        assert_eq!(v["markets"][0]["ok"], false);
        // A market the chain does not have.
        let mut r: Value = serde_json::from_str(&reads(&m, &fv(false, 0))).unwrap();
        r["results"]["count"] = json!(format!("0x{}", w(0)));
        let v: Value =
            serde_json::from_str(&live(&config_json(), &r.to_string(), 0).unwrap()).unwrap();
        assert_eq!(v["markets"][0]["ok"], false);
    }

    #[test]
    fn decimals_and_buckets_match_the_contract() {
        assert_eq!(decimal(283, 1), "28.3");
        assert_eq!(decimal(-5, 2), "-0.05");
        assert_eq!(decimal(9, 0), "9");
        assert_eq!(bucket_of(&EDGES, EDGES[0] - 1), 0);
        assert_eq!(
            bucket_of(&EDGES, EDGES[0]),
            1,
            "an edge belongs to the bucket above"
        );
        assert_eq!(bucket_of(&EDGES, EDGES[1] + 1), 2);
    }

    const F1: &str = "0xc1ba8d6dad265d88506a37094c709084b4a98366e0ac1dba7e252bd6563e23ef";
    const F2: &str = "0xbcec4881cd913049dffb6463a010052534adc47d99d6cf2600096b194b09eeee";
    /// `product(1)` and `product(2)` as Base Sepolia returned them on 2026-10-28 (open, none sold).
    const P1: &str = "0xc1ba8d6dad265d88506a37094c709084b4a98366e0ac1dba7e252bd6563e23ef0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000006ae52f80000000000000000000000000000000000000000000000000000000006b0e0e00000000000000000000000000000000000000000000000000000000006b359b0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000056bc75e2d631000000000000000000000000000000000000000000000000000008ac7230489e800000000000000000000000000000000000000000000000000000000000000000000";
    const P2: &str = "0xbcec4881cd913049dffb6463a010052534adc47d99d6cf2600096b194b09eeee0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000006ae52f80000000000000000000000000000000000000000000000000000000006b6a5700000000000000000000000000000000000000000000000000000000006b84b5000000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000002b5e3af16b1880000000000000000000000000000000000000000000000000000d02ab486cedc00000000000000000000000000000000000000000000000000000000000000000000";

    fn ins_config() -> String {
        let mut c: Value = serde_json::from_str(&config_json()).unwrap();
        c["insurance"] = json!({
            "address": "0xfc585f41e5e615b774a24404fd5a857042f63639",
            "facts": "0x8d10fc8f37875f15f4d180dbcaa6c2915334bb76",
            "products": [{"id": 1, "fact": F1, "scale": 1}, {"id": 2, "fact": F2, "scale": 0}],
        });
        c.to_string()
    }

    fn ins_reads(p1: &str, p2: &str, f1: &str, pool: u128, reserved: u128) -> String {
        let m = encode(FACT, 1, 0, 0, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let mut r: Value = serde_json::from_str(&reads(&m, &fv(false, 0))).unwrap();
        let res = &mut r["results"];
        res["ins:count"] = json!(format!("0x{}", w(2)));
        res["ins:pool"] = json!(format!("0x{}", w(i128::try_from(pool).unwrap())));
        res["ins:reserved"] = json!(format!("0x{}", w(i128::try_from(reserved).unwrap())));
        res["product:1"] = json!(p1);
        res["product:2"] = json!(p2);
        res["pfact:1"] = json!(f1);
        res["pfact:2"] = json!(fv(false, 0));
        r.to_string()
    }

    #[test]
    fn insurance_calls_name_the_policy_reads() {
        let v: Value = serde_json::from_str(&calls(&ins_config()).unwrap()).unwrap();
        let keys: Vec<&str> = v["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["key"].as_str().unwrap())
            .collect();
        assert!(
            keys.contains(&"ins:pool") && keys.contains(&"product:2") && keys.contains(&"pfact:1")
        );
        let p = v["calls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == "product:1")
            .unwrap();
        assert!(p["data"].as_str().unwrap().starts_with("0x780bd3f1"));
        assert_eq!(p["to"], "0xfc585f41e5e615b774a24404fd5a857042f63639");
    }

    /// The two live demo products: open, nothing sold, nothing held back.
    #[test]
    fn the_live_products_decode() {
        let v: Value = serde_json::from_str(
            &live(
                &ins_config(),
                &ins_reads(P1, P2, &fv(false, 0), 0, 0),
                1_793_000_000,
            )
            .unwrap(),
        )
        .unwrap();
        let i = &v["insurance"];
        assert_eq!(i["ok"], true, "{i}");
        assert_eq!(i["products"][0]["cmp"], "atmost");
        assert_eq!(i["products"][0]["threshold"], "0.0");
        assert_eq!(i["products"][0]["payout"], "100.000000000000000000");
        assert_eq!(i["products"][0]["premium"], "10.000000000000000000");
        assert_eq!(i["products"][0]["maxPolicies"], 20);
        assert_eq!(i["products"][0]["buying"], true);
        assert_eq!(i["products"][1]["cmp"], "atleast");
        assert_eq!(i["products"][1]["threshold"], "6");
        assert_eq!(i["products"][1]["buyBy"], 1_793_404_800);
    }

    /// A paid product must hold a level-2 value that meets its threshold;
    /// an expired one a value that does not.
    #[test]
    fn a_settled_policy_must_match_the_value() {
        // Product 1 at state 2 (paid) with value -12 (Denver at most 0.0 °C: -1.2 meets it).
        let paid = format!(
            "{}{}{}{}",
            &P1[..2 + 64 * 2],
            w(2),
            &P1[2 + 64 * 3..2 + 64 * 11],
            w(-12)
        );
        let ok = live(
            &ins_config(),
            &ins_reads(&paid, P2, &fv(true, -12), 100 * T, 0),
            1_796_100_000,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["insurance"]["products"][0]["ok"], true, "{v}");
        assert_eq!(v["insurance"]["products"][0]["value"], "-1.2");
        // Paid, but the value does not meet the threshold.
        let wrong = paid.replace(&w(-12), &w(15));
        let v: Value = serde_json::from_str(
            &live(
                &ins_config(),
                &ins_reads(&wrong, P2, &fv(true, 15), 100 * T, 0),
                0,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["insurance"]["products"][0]["ok"], false);
        // Paid with no value in the facts.
        let v: Value = serde_json::from_str(
            &live(
                &ins_config(),
                &ins_reads(&paid, P2, &fv(false, 0), 100 * T, 0),
                0,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["insurance"]["products"][0]["ok"], false);
    }

    #[test]
    fn another_fact_or_an_uncovered_pool_is_refused() {
        let other = P1.replacen(&F1[2..], &"33".repeat(32), 1);
        let v: Value = serde_json::from_str(
            &live(
                &ins_config(),
                &ins_reads(&other, P2, &fv(false, 0), 0, 0),
                0,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["insurance"]["products"][0]["ok"], false);
        let v: Value = serde_json::from_str(
            &live(
                &ins_config(),
                &ins_reads(P1, P2, &fv(false, 0), 10 * T, 11 * T),
                0,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(v["insurance"]["ok"], false, "held back more than the pool");
        assert_eq!(v["ok"], false);
        assert!(meets(0, 6, 6) && !meets(0, 5, 6) && meets(1, 0, 0) && !meets(1, 1, 0));
    }

    const G1: &str = "0x3dd92d8cbf2890ecfd7cfc0c1d88b223ef5f3498995480736187ed9bfd9e2792";

    /// `market(1)` of the guarded futures as Base Sepolia answered on
    /// 2026-10-29 (market 2 on the page: four bets, open; NL23).
    const GUARDED_1: &str = concat!(
        "0x00000000000000000000000000000000000000000000000000000000000000203dd92d8cbf2890ecfd7cfc0c1d88b2",
        "23ef5f3498995480736187ed9bfd9e2792000000000000000000000000000000000000000000000000000000006ae7c4",
        "70000000000000000000000000000000000000000000000000000000006aea7580000000000000000000000000000000",
        "000000000000000000000000006af3b00000000000000000000000000000000000000000000000000000000000000000",
        "010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "040000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000068155a43676e0000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000000220000000000000000000000000000000",
        "000000000000000000000000000000028000000000000000000000000000000000000000000000000000000000000003",
        "000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000",
        "000000000000000000000e6c8c69c9c000000000000000000000000000000000000000000000000000000e75a4b83c60",
        "000000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000",
        "000000000000000001a055690d9db80000000000000000000000000000000000000000000000000003cb71f51fc55800",
        "00000000000000000000000000000000000000000000000001158e460913d00000000000000000000000000000000000",
        "000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000000",
        "010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000",
        "0000000000000000000000000000000001",
    );

    fn guarded_config() -> String {
        let mut c: Value = serde_json::from_str(&ins_config()).unwrap();
        c["insurance"]["futures"] = json!("0x07e89ae47579f9297c28ccab782acf9443354a65");
        c["insurance"]["markets"] = json!([{"id": 1, "fact": G1, "scale": 2}]);
        c.to_string()
    }

    fn guarded_reads(market: &str, count: i128) -> String {
        let mut r: Value = serde_json::from_str(&ins_reads(P1, P2, &fv(false, 0), 0, 0)).unwrap();
        r["results"]["gcount"] = json!(format!("0x{}", w(count)));
        r["results"]["gmarket:1"] = json!(market);
        r["results"]["gfact:1"] = json!(fv(false, 0));
        r.to_string()
    }

    /// A market on the guarded futures is read from those futures and the
    /// guarded facts, never from the first futures (NL23).
    #[test]
    fn guarded_markets_are_read_from_the_guarded_futures() {
        let v: Value = serde_json::from_str(&calls(&guarded_config()).unwrap()).unwrap();
        let find = |k: &str| {
            v["calls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["key"] == k)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            find("gcount")["to"],
            "0x07e89ae47579f9297c28ccab782acf9443354a65"
        );
        assert_eq!(
            find("gmarket:1")["to"],
            "0x07e89ae47579f9297c28ccab782acf9443354a65"
        );
        assert_eq!(
            find("gfact:1")["to"],
            "0x8d10fc8f37875f15f4d180dbcaa6c2915334bb76"
        );
        // A file without guarded markets reads nothing more than before.
        let plain: Value = serde_json::from_str(&calls(&ins_config()).unwrap()).unwrap();
        assert!(!plain.to_string().contains("gcount"));
    }

    #[test]
    fn the_live_guarded_market_decodes() {
        let v: Value = serde_json::from_str(
            &live(
                &guarded_config(),
                &guarded_reads(GUARDED_1, 1),
                1_793_300_000,
            )
            .unwrap(),
        )
        .unwrap();
        let g = &v["insurance"]["markets"][0];
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(g["state"], "open");
        assert_eq!(g["trading"], true);
        assert_eq!(g["bettors"], 4);
        assert_eq!(g["tradingClose"], 1_793_574_000);
        assert_eq!(g["observableAt"], 1_793_750_400);
        // The first futures' market 1 is still read on its own keys.
        assert_eq!(v["markets"][0]["ok"], true);
    }

    /// A guarded market the chain does not have, or one naming another fact,
    /// is refused, and so is the whole read.
    #[test]
    fn a_missing_or_foreign_guarded_market_is_refused() {
        let none: Value = serde_json::from_str(
            &live(
                &guarded_config(),
                &guarded_reads(GUARDED_1, 0),
                1_793_300_000,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(none["insurance"]["markets"][0]["ok"], false);
        assert_eq!(none["ok"], false);
        let other = encode(FACT, 1, 0, 0, &EDGES, &[T, T, T]);
        let foreign: Value = serde_json::from_str(
            &live(&guarded_config(), &guarded_reads(&other, 1), 1_793_300_000).unwrap(),
        )
        .unwrap();
        assert_eq!(foreign["insurance"]["markets"][0]["ok"], false);
        let mut r: Value = serde_json::from_str(&guarded_reads(GUARDED_1, 1)).unwrap();
        r["results"].as_object_mut().unwrap().remove("gcount");
        let lost: Value =
            serde_json::from_str(&live(&guarded_config(), &r.to_string(), 1_793_300_000).unwrap())
                .unwrap();
        assert_eq!(lost["insurance"]["ok"], false);
    }

    const CORRECTIONS: &str = "0x3765dab41115fc2b849b13f81c75fce96a771133";
    const C1: &str = "0xeae3ecdef95a96f5641568a96510c3b779a520f081ed4b21c8ab0532c03f665b";
    /// `market(1)` of `OnyxCorrections` as Base Sepolia answered on 2026-11-15
    /// at block 59100187 (open, Yes 10 and No 10 tONYX; CL7).
    const CORRECTION_1: &str = concat!(
        "0x",
        "7061ad61eb459785b9fd7cc8920522a1d6d93ffc5bf72cd13af5a8eab8cec910",
        "dc8cf69fe2743a2becbe155e02b17e66fae85a67f1efbca4135c137a8ea0e537",
        "eae3ecdef95a96f5641568a96510c3b779a520f081ed4b21c8ab0532c03f665b",
        "06f535ad96ee4f508671bc010f2a62df1b7d0d124385af379fa6f5fd22bf4f89",
        "00000000000000000000000034fb3097a51b7ede8964d89bd7895c81d772d912",
        "000000000000000000000000000000000000000000000000000000006ae42267",
        "00000000000000000000000000000000000000000000000000000000038fd843",
        "0000000000000000000000000000000000000000000000000000000003ad8203",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000002",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "000000000000000000000000000000000000000000000000000000000000481f",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000004563918244f40000",
        "000000000000000000000000000000000000000000000001158e460913d00000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000008ac7230489e80000",
        "0000000000000000000000000000000000000000000000008ac7230489e80000",
        "0000000000000000000000000000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000000000000000000000000000001",
    );
    /// Block 59098486 and its time (2026-11-15 11:19:26 UTC), 0.2 s blocks.
    const HEAD: u64 = 59_098_486;
    const NOW: u64 = 1_794_741_566;

    fn corr_config() -> String {
        let mut c: Value = serde_json::from_str(&config_json()).unwrap();
        c["corrections"] = json!({
            "address": CORRECTIONS,
            "blockMs": 200,
            "markets": [{"id": 1, "fact": C1, "metric": "us.treasury.tga_total_deposits", "scale": 0}],
        });
        c.to_string()
    }

    /// `hexs` with word `i` (of a static return) set to `word`.
    fn set_word(hexs: &str, i: usize, word: &str) -> String {
        let at = 2 + 64 * i;
        format!("{}{}{}", &hexs[..at], word, &hexs[at + 64..])
    }

    fn set(hexs: &str, i: usize, n: i128) -> String {
        set_word(hexs, i, &w(n))
    }

    fn corr_reads(market: &str, count: i128, fee: i128, block: Option<u64>) -> String {
        let m = encode(FACT, 1, 0, 0, &EDGES, &[30 * T, 75 * T, 20 * T]);
        let mut r: Value = serde_json::from_str(&reads(&m, &fv(false, 0))).unwrap();
        if let Some(b) = block {
            r["block"] = json!(format!("0x{b:x}"));
        }
        r["results"]["corr:count"] = json!(format!("0x{}", w(count)));
        r["results"]["corr:fee"] = json!(format!("0x{}", w(fee)));
        r["results"]["correction:1"] = json!(market);
        r.to_string()
    }

    fn corr_live(market: &str) -> Value {
        corr_live_at(market, HEAD)
    }

    /// Market 1 read at the page's block `head`.
    fn corr_live_at(market: &str, head: u64) -> Value {
        let out = live(
            &corr_config(),
            &corr_reads(market, 1, 20_000, Some(head)),
            NOW,
        )
        .unwrap();
        serde_json::from_str(&out).unwrap()
    }

    /// A block after market 1's close (59,758,659) and before its deadline.
    const AFTER_CLOSE: u64 = 59_800_000;

    /// Market 1's problems at `head`, joined ("" when it passes).
    fn problems_at(market: &str, head: u64) -> String {
        let v = corr_live_at(market, head);
        let m = &v["corrections"]["markets"][0];
        let list: Vec<&str> = m["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        assert_eq!(m["ok"] == json!(true), list.is_empty(), "{v}");
        list.join("; ")
    }

    /// `hexs` with words `(index, value)` set.
    fn with(hexs: &str, words: &[(usize, i128)]) -> String {
        words
            .iter()
            .fold(hexs.to_string(), |h, (i, n)| set(&h, *i, *n))
    }

    fn t(n: u128) -> i128 {
        i128::try_from(n).unwrap()
    }

    #[test]
    fn correction_calls_name_the_reads() {
        let v: Value = serde_json::from_str(&calls(&corr_config()).unwrap()).unwrap();
        let find = |k: &str| {
            v["calls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["key"] == k)
                .cloned()
                .unwrap()
        };
        assert_eq!(find("corr:count")["data"], "0xec979082");
        assert_eq!(find("corr:fee")["data"], "0xa534150c");
        let m = find("correction:1");
        assert_eq!(m["to"], CORRECTIONS);
        assert_eq!(m["data"], format!("0x28861d22{}", w(1)));
        // A file without corrections reads nothing more than before.
        let plain: Value = serde_json::from_str(&calls(&config_json()).unwrap()).unwrap();
        assert!(!plain.to_string().contains("corr"));
    }

    /// The live market 1: open, Yes 10 and No 10 tONYX, close and deadline
    /// blocks at about 2026-11-17 00:00 and 2026-11-21 12:00 UTC.
    #[test]
    fn the_live_correction_market_1_decodes() {
        let v = corr_live(CORRECTION_1);
        let c = &v["corrections"];
        let m = &c["markets"][0];
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(c["feePpm"], 20_000);
        assert_eq!(m["state"], "open");
        assert_eq!(m["betting"], true);
        assert_eq!(m["pastDeadline"], false);
        assert_eq!(m["value"], "18463");
        assert_eq!(m["tolerance"], "0");
        assert_eq!(m["yes"]["pool"], "10.000000000000000000");
        assert_eq!(m["no"]["pool"], "10.000000000000000000");
        assert_eq!(m["yes"]["bettors"], 1);
        assert_eq!(m["no"]["sharePpm"], "500000");
        assert_eq!(m["total"], "20.000000000000000000");
        assert_eq!(m["bettors"], 2);
        assert_eq!(m["closeBlock"], 59_758_659);
        assert_eq!(m["resolveBlock"], 61_702_659);
        assert_eq!(m["closeAt"], 1_794_873_600, "2026-11-17 00:00 UTC");
        assert_eq!(m["resolveAt"], 1_795_262_400, "2026-11-21 12:00 UTC");
        assert_eq!(m["recheckValue"], Value::Null);
        assert_eq!(m["winner"], Value::Null);
        // At the close block betting is over; past the deadline too.
        let closed = live(
            &corr_config(),
            &corr_reads(CORRECTION_1, 1, 20_000, Some(59_758_659)),
            NOW,
        )
        .unwrap();
        let closed: Value = serde_json::from_str(&closed).unwrap();
        assert_eq!(closed["corrections"]["markets"][0]["betting"], false);
        assert_eq!(closed["corrections"]["markets"][0]["pastDeadline"], false);
        let late = live(
            &corr_config(),
            &corr_reads(CORRECTION_1, 1, 20_000, Some(61_702_660)),
            NOW,
        )
        .unwrap();
        let late: Value = serde_json::from_str(&late).unwrap();
        assert_eq!(late["corrections"]["markets"][0]["pastDeadline"], true);
    }

    /// Market 1 resolved at `recheck` with `winner`, every winner paid: the
    /// losing side (10 tONYX) less the 2% fee is 9.8 tONYX for the winners;
    /// `left` and `winnersLeft` are back to 0.
    fn resolved(recheck: i128, winner: i128, distributable: u128) -> String {
        with(
            CORRECTION_1,
            &[
                (8, 2),
                (9, winner),
                (14, recheck),
                (18, t(10 * T)),
                (19, t(distributable)),
            ],
        )
    }

    const PAY: u128 = 98 * T / 10;

    #[test]
    fn a_resolved_correction_must_pay_what_the_recheck_says() {
        // The recheck agrees (tolerance 0): Yes, "accurate", wins.
        let v = corr_live_at(&resolved(18_463, 1, PAY), AFTER_CLOSE);
        let m = &v["corrections"]["markets"][0];
        assert_eq!(m["ok"], true, "{v}");
        assert_eq!(m["winner"], "yes");
        assert_eq!(m["recheckValue"], "18463");
        assert_eq!(m["betting"], false);
        // The recheck disagrees by one: No wins.
        let v = corr_live_at(&resolved(18_464, 2, PAY), AFTER_CLOSE);
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["corrections"]["markets"][0]["winner"], "no");
        // The winner is not the recheck's side.
        let wrong = "the winner is not what the recheck value and the tolerance say";
        assert_eq!(problems_at(&resolved(18_464, 1, PAY), AFTER_CLOSE), wrong);
        assert_eq!(
            corr_live_at(&resolved(18_464, 1, PAY), AFTER_CLOSE)["ok"],
            false
        );
        // Resolved, but it also names a void reason.
        assert_eq!(
            problems_at(&set(&resolved(18_463, 1, PAY), 10, 1), AFTER_CLOSE),
            wrong
        );
        // Winners paid more than the losing side less the fee.
        let payout = "the winners' pool or payout is not the pools less the fee";
        assert_eq!(
            problems_at(&resolved(18_463, 1, 10 * T), AFTER_CLOSE),
            payout
        );
        // The winners' pool is not the winning side's.
        assert_eq!(
            problems_at(&set(&resolved(18_463, 1, PAY), 18, t(9 * T)), AFTER_CLOSE),
            payout
        );
        // The winning side is empty (the contract voids that, reason 1).
        let empty = with(
            &resolved(18_463, 1, 0),
            &[(21, 0), (23, 0), (17, t(10 * T)), (11, 1), (18, 0)],
        );
        assert_eq!(problems_at(&empty, AFTER_CLOSE), payout);
        // With a tolerance of 1 the same disagreement is accurate.
        let m = set(&resolved(18_464, 1, PAY), 15, 1);
        let v = corr_live_at(&m, AFTER_CLOSE);
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["corrections"]["markets"][0]["tolerance"], "1");
    }

    /// `resolve` reverts before the close block, so an answer settled from a
    /// recheck (resolved, or void because the winning side had no bet) at an
    /// earlier block is refused; a void for no valid recheck may come early.
    #[test]
    fn a_market_settled_before_its_close_is_refused() {
        let early = "settled from a recheck before its close block";
        assert_eq!(problems_at(&resolved(18_463, 1, PAY), HEAD), early);
        assert_eq!(problems_at(&resolved(18_463, 1, PAY), 59_758_658), early);
        assert_eq!(
            problems_at(&resolved(18_463, 1, PAY), 59_758_659),
            "",
            "at the close block"
        );
        let nobet = with(
            CORRECTION_1,
            &[
                (8, 3),
                (10, 1),
                (9, 1),
                (14, 18_463),
                (21, 0),
                (23, 0),
                (17, t(10 * T)),
                (11, 1),
            ],
        );
        assert_eq!(problems_at(&nobet, HEAD), early);
        assert_eq!(problems_at(&nobet, AFTER_CLOSE), "");
        assert_eq!(
            problems_at(&with(CORRECTION_1, &[(8, 3), (10, 2)]), HEAD),
            ""
        );
    }

    #[test]
    fn a_void_correction_names_its_reason() {
        let none = set(CORRECTION_1, 8, 3);
        let v = corr_live(&set(&none, 10, 2));
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["corrections"]["markets"][0]["voidReason"], "norecheck");
        assert_eq!(v["corrections"]["markets"][0]["recheckValue"], Value::Null);
        // No valid recheck, but a winner or a recheck value.
        let norecheck = "void for no valid recheck, but it names a winner or a recheck value";
        assert_eq!(
            problems_at(&with(&none, &[(10, 2), (9, 1)]), HEAD),
            norecheck
        );
        assert_eq!(
            problems_at(&with(&none, &[(10, 2), (14, 18_463)]), HEAD),
            norecheck
        );
        // "The winning side had no bet", but both sides hold bets.
        let nobet = with(&none, &[(10, 1), (9, 1), (14, 18_463)]);
        assert_eq!(
            problems_at(&nobet, AFTER_CLOSE),
            "void because the winning side had no bet, but it has bets"
        );
        // It is right once the winning side (Yes) is empty.
        let empty = with(&nobet, &[(21, 0), (23, 0), (17, t(10 * T)), (11, 1)]);
        let v = corr_live_at(&empty, AFTER_CLOSE);
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["corrections"]["markets"][0]["voidReason"], "nobet");
        assert_eq!(v["corrections"]["markets"][0]["recheckValue"], "18463");
        // No reason, or one the contract does not have.
        assert_eq!(problems_at(&none, HEAD), "void reason 0");
        assert_eq!(problems_at(&set(&none, 10, 3), HEAD), "void reason 3");
    }

    /// Every state the contract can reach passes: open; resolved right after
    /// `resolve`, after some and after all withdrawals (the dust goes to the
    /// treasury with the last one); void for either reason right after `void`,
    /// after some and after all refunds.
    #[test]
    fn every_reachable_payout_state_passes() {
        let fresh = with(&resolved(18_463, 1, PAY), &[(20, t(10 * T + PAY)), (12, 1)]);
        for (what, m, head) in [
            ("open", CORRECTION_1.to_string(), HEAD),
            ("resolved, nobody paid", fresh, AFTER_CLOSE),
            (
                "resolved, every winner paid",
                resolved(18_463, 1, PAY),
                AFTER_CLOSE,
            ),
            (
                "void, no refund yet",
                with(CORRECTION_1, &[(8, 3), (10, 2), (20, t(20 * T)), (12, 2)]),
                HEAD,
            ),
            (
                "void, one refund",
                with(CORRECTION_1, &[(8, 3), (10, 2), (20, t(10 * T)), (12, 1)]),
                AFTER_CLOSE,
            ),
            (
                "void, every refund",
                with(CORRECTION_1, &[(8, 3), (10, 2)]),
                61_800_000,
            ),
        ] {
            assert_eq!(problems_at(&m, head), "", "{what}");
        }
    }

    /// Payout fields the contract cannot hold in that state are refused.
    #[test]
    fn impossible_payout_fields_are_refused() {
        let open = "an open market already holds a recheck, a winner or a payout";
        for word in [12, 18, 19, 20] {
            assert_eq!(
                problems_at(&set(CORRECTION_1, word, 1), HEAD),
                open,
                "word {word}"
            );
        }
        assert_eq!(
            problems_at(&set(&resolved(18_463, 1, PAY), 12, 2), AFTER_CLOSE),
            "more winners left to pay than bet on the winning side"
        );
        assert_eq!(
            problems_at(
                &set(&resolved(18_463, 1, PAY), 20, t(10 * T + PAY + 1)),
                AFTER_CLOSE
            ),
            "more left to pay than the winners' pool and payout"
        );
        let void = with(CORRECTION_1, &[(8, 3), (10, 2)]);
        let refund = "more left to refund than was bet";
        assert_eq!(problems_at(&set(&void, 20, t(20 * T + 1)), HEAD), refund);
        assert_eq!(problems_at(&set(&void, 12, 3), HEAD), refund);
    }

    /// A `uint128` tolerance is shown exactly, up to its largest value.
    #[test]
    fn a_tolerance_is_shown_exactly() {
        let max = set_word(
            CORRECTION_1,
            15,
            &format!("{}{}", "00".repeat(16), "ff".repeat(16)),
        );
        let v = corr_live(&max);
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(
            v["corrections"]["markets"][0]["tolerance"],
            u128::MAX.to_string()
        );
        assert_eq!(
            decimal_u(u128::MAX, 0),
            "340282366920938463463374607431768211455"
        );
        assert_eq!(
            tokens(u128::MAX),
            "340282366920938463463.374607431768211455"
        );
        assert_eq!(decimal_u(5, 2), "0.05");
    }

    /// A negative value (a sign-extended word) is accepted and keeps its sign.
    #[test]
    fn a_negative_value_keeps_its_sign() {
        let v = corr_live(&set(CORRECTION_1, 13, -18_463));
        assert_eq!(v["corrections"]["markets"][0]["ok"], true, "{v}");
        assert_eq!(v["corrections"]["markets"][0]["value"], "-18463");
    }

    #[test]
    fn another_fact_metric_or_malformed_words_are_refused() {
        let refused = |m: &str| corr_live(m)["corrections"]["markets"][0]["ok"] == json!(false);
        let other = format!("0x{}", "44".repeat(32));
        assert!(
            refused(&CORRECTION_1.replacen(&C1[2..], &other[2..], 1)),
            "another fact"
        );
        assert!(
            refused(&CORRECTION_1.replacen("7061ad61", "7061ad62", 1)),
            "another metric"
        );
        assert!(
            refused(&CORRECTION_1[..CORRECTION_1.len() - 64]),
            "24 words"
        );
        assert!(refused(&format!("{CORRECTION_1}{}", w(0))), "26 words");
        assert!(refused(&set(CORRECTION_1, 4, -1)), "word 4 is no address");
        let unsigned = format!("{}80{}", "00".repeat(16), "00".repeat(15));
        assert!(
            refused(&set_word(CORRECTION_1, 13, &unsigned)),
            "a value word not sign-extended"
        );
        let high = format!("01{}", "00".repeat(31));
        for word in [12, 16, 20] {
            assert!(
                refused(&set_word(CORRECTION_1, word, &high)),
                "high bits in word {word}"
            );
        }
        assert!(
            refused(&set(CORRECTION_1, 12, 1 << 32)),
            "winnersLeft over 32 bits"
        );
        assert!(
            refused(&set(CORRECTION_1, 5, 1 << 64)),
            "windowEnd over 64 bits"
        );
        assert!(
            refused(&set(CORRECTION_1, 11, 3)),
            "three bettors on two one-bettor sides"
        );
        assert!(
            refused(&set(CORRECTION_1, 23, 0)),
            "a pool with money and no bettor"
        );
        assert!(
            refused(&set(CORRECTION_1, 17, i128::try_from(21 * T).unwrap())),
            "pools that miss the total"
        );
        assert!(
            refused(&set(CORRECTION_1, 6, 61_702_659)),
            "a close at the deadline"
        );
        assert!(
            refused(&set(CORRECTION_1, 9, 1)),
            "an open market with a winner"
        );
        assert!(refused(&set(CORRECTION_1, 8, 4)), "state 4");
        // A market the chain does not have, no block, a fee over 10%: refused.
        let count0 = live(
            &corr_config(),
            &corr_reads(CORRECTION_1, 0, 20_000, Some(HEAD)),
            NOW,
        )
        .unwrap();
        assert!(count0.contains("does not exist on chain"));
        let nob: Value = serde_json::from_str(
            &live(
                &corr_config(),
                &corr_reads(CORRECTION_1, 1, 20_000, None),
                NOW,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(nob["corrections"]["ok"], false);
        assert_eq!(nob["ok"], false);
        let fee: Value = serde_json::from_str(
            &live(
                &corr_config(),
                &corr_reads(CORRECTION_1, 1, 100_001, Some(HEAD)),
                NOW,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fee["corrections"]["ok"], false);
        // The futures and their checks are untouched by the corrections.
        assert_eq!(nob["markets"][0]["ok"], true);
    }

    #[test]
    fn block_times_are_estimates_within_what_a_page_shows() {
        assert_eq!(block_time(HEAD, HEAD, 200, NOW), Some(NOW));
        assert_eq!(block_time(HEAD - 5, HEAD, 200, NOW), Some(NOW - 1));
        assert_eq!(block_time(u64::MAX, HEAD, 200, NOW), None, "past year 9999");
        assert_eq!(block_time(0, u64::MAX, 60_000, 0), None, "before 1970");
        assert_eq!(read_block(&json!("0x385cbab")), Some(59_100_075));
        for bad in [
            json!("385fd76"),
            json!("0x"),
            json!("0x+1"),
            json!(format!("0x{}", "f".repeat(17))),
            json!(59),
        ] {
            assert_eq!(read_block(&bad), None, "{bad}");
        }
        assert_eq!(payout_fee(10 * T, 20_000), T / 5);
        assert_eq!(payout_fee(u128::MAX, 100_000), u128::MAX / 10);
        assert_eq!(correction_side(18_463, 18_463, 0), YES);
        assert_eq!(correction_side(i128::MIN, i128::MAX, u128::MAX), YES);
        assert_eq!(correction_side(-1, 1, 1), NO);
    }
}
