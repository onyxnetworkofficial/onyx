//! The recheck runner of correction markets (docs/contributor-loop.md CL7, ADR-0057 item 7;
//! THREATS CL7-3, CL7-13, CL7-R1-1). For one value of a dated source it reads the value's
//! markets and every bettor from `OnyxCorrections` (`marketsOfRecheck`, `bettorsOf`: no log
//! scan, which a public endpoint's 200-block pages make impractical) and the original round's
//! observers from its results store, then plans the recheck round: the markets it serves
//! ([`serve`]), the first window the recheck allows after their latest close
//! ([`recheck_window_start`]), and the pool without any bettor or original observer
//! ([`recheck_pool`]). The round itself is the coordinator's, run on the paired recheck source
//! (`level2-manifest coordinator --recheck A:S --pool <plan pool> --window-start <plan start>`);
//! `resolve` then settles every market the recorded recheck can settle.
//!
//! Commands (`examples/level2-manifest.rs`, with `--recheck AFTER_DAYS:SPAN_DAYS`):
//! - `recheck-plan --record-date D --rpc URL --corrections ADDR --pool FILE --original-store FILE
//!   [--out-pool FILE] [--margin-blocks N]`: the plan as JSON; `"state"` is `none` (no open
//!   market), `wait` (trading still open), `void-only` (no recheck day left, CL7-13) or `run`
//!   (with `windowStart`, the served markets and the pool written to `--out-pool`).
//!   While trading is open (`wait`) a `--pool` gives a preview too: `pool` and `excluded` without
//!   the bettors so far and the original observers (written to `--out-pool`), and the round's
//!   `minObservers`: an operator's pre-flight, `"provisional": true` since bettors may still join
//!   until the close.
//! - `recheck-resolve --record-date D --rpc URL --corrections ADDR --store FILE --key-file F
//!   [--dry-run] [--margin-blocks N]`: `resolve` every open market of the value the round serves
//!   (as the plan, `leftToVoid` the others) from the recheck's results store (anyone may). On
//!   a local anvil it sends from an unlocked account instead (`--from ADDR`: no key there).
//!   `--dry-run` sends nothing: each `resolve` is estimated (`eth_estimateGas`).
//! - `recheck-payout --record-date D --rpc URL --corrections ADDR --key-file F [--key-file F ...]
//!   [--treasury-key-file F] [--dry-run]`: for every settled (resolved or void) market of the value,
//!   `withdraw` for each key owed something (`owed`: a winner, or any bettor of a void market; a
//!   key already paid is reported and skipped), then `payTreasury()` from `--treasury-key-file`
//!   when the treasury is owed anything. Every step reads the chain first, so a second run sends
//!   nothing. On a local anvil `--from ADDR` and `--treasury-from ADDR` (unlocked) instead.

use crate::live::{flag, hex, need, KeySigner};
use crate::markets::{ax, block_now, block_time, call, num, send, signer_of};
use crate::{parse_pool, Source};
use onyx_api::observations::{
    check_registered, registry_metric_calldata, subject_text, ObservationStore, TemplatePins,
};
use onyx_coordinator::cast_chain::{parse_addr, CastChain, TxSigner, ANVIL_CHAIN_ID};
use onyx_coordinator::corrections::{recheck_pool, recheck_window_start, serve, OpenMarket};
use onyx_coordinator::observe::{check_pool, Observer};
use onyx_core::onchain::{metric_key, registry_fact_key, subject_key};
use onyx_core::{recheck_of, Answer, FeedTemplate, Hash32, Subject, SubjectRule};
use serde_json::{json, Value as Json};

/// `OnyxCorrections.market(id)`: the `Market` struct, decoded by `cast call --json`.
const MARKET_SIG: &str = "market(uint256)((bytes32,bytes32,bytes32,bytes32,address,uint64,uint64,uint64,uint8,uint8,uint8,uint32,uint32,int128,int128,uint128,uint128,uint128,uint128,uint128,uint128,uint128[2],uint32[2]))";

/// Ids or addresses per `marketsOfRecheck` / `bettorsOf` call.
const PAGE: u64 = 200;

/// Blocks a recheck round needs from its draw to its record when `--margin-blocks` is not
/// given: 15,000 (50 minutes at Base Sepolia's 0.2 s blocks). A live round takes 35 to 40
/// minutes from its draw (the operator's lead, the 300 s window, commit and reveal, then about
/// 20 minutes until the commit phase is final), so a market whose close leaves less than that
/// before the earliest deadline is left to void rather than delay the others (CL7-R1-1). At most
/// the deployment's `minRecheckBlocks` in practice.
pub const DEFAULT_MARGIN_BLOCKS: u64 = 15_000;

/// `Market.state` of an open market.
const OPEN: u128 = 1;

/// `Market.state` names (`OnyxCorrections.State`).
fn state_name(state: u128) -> &'static str {
    match state {
        1 => "open",
        2 => "resolved",
        3 => "void",
        _ => "unknown",
    }
}

/// The subject `date` names under a dated, period or daily rule, or under a recheck of one.
pub fn subject_of(t: &FeedTemplate, date: &str) -> Result<Subject, String> {
    let rule = match &t.subject {
        SubjectRule::Recheck { of, .. } => of.as_ref(),
        rule => rule,
    };
    let prefix = match rule {
        SubjectRule::Dated { prefix, .. }
        | SubjectRule::Period { prefix, .. }
        | SubjectRule::Daily { prefix, .. } => prefix,
        _ => return Err("a correction market names a dated, period or daily value".into()),
    };
    onyx_core::day_number(date).ok_or(format!("{date}: not a YYYY-MM-DD date"))?;
    Ok(Subject::Aggregate(format!("{prefix}:{date}")))
}

/// The registry fact of `subject` under `t` (dated facts use window 0, `OnyxRegistry.factKeyOf`).
fn fact_of(t: &FeedTemplate, subject: &Subject) -> Hash32 {
    registry_fact_key(
        &metric_key(&t.metric),
        &t.id(),
        &subject_key(subject),
        0,
        true,
    )
}

fn h(x: &Hash32) -> String {
    format!("0x{}", hex(&x.0))
}

fn word(v: &Json, what: &str) -> Result<u64, String> {
    u64::try_from(num(v, what)?).map_err(|e| format!("{what}: {e}"))
}

/// A read-only call at `block` (`cast call --block N`), or at the head with `None`.
fn call_at(
    chain: &CastChain,
    to: &[u8; 20],
    sig: &str,
    args: &[String],
    block: Option<u64>,
) -> Result<Vec<Json>, String> {
    let Some(b) = block else {
        return call(chain, to, sig, args);
    };
    let mut a = args.to_vec();
    a.extend(["--block".into(), b.to_string()]);
    // A public endpoint balances reads over nodes; one a block behind does not hold `b` yet.
    let mut last = String::new();
    for _ in 0..5 {
        match call(chain, to, sig, &a) {
            Ok(r) => return Ok(r),
            Err(e) => last = e,
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    Err(last)
}

/// One page-wise list read at `block`: `(items, total)` pages of `sig` until `total` items are
/// read.
fn paged(
    chain: &CastChain,
    to: &[u8; 20],
    sig: &str,
    key: &str,
    what: &str,
    block: Option<u64>,
) -> Result<Vec<Json>, String> {
    let mut out = Vec::new();
    loop {
        let r = call_at(
            chain,
            to,
            sig,
            &[key.to_owned(), out.len().to_string(), PAGE.to_string()],
            block,
        )?;
        let items = r
            .first()
            .and_then(Json::as_array)
            .ok_or(format!("{what}: no list"))?;
        let total = word(r.get(1).ok_or(format!("{what}: no total"))?, what)?;
        out.extend(items.iter().cloned());
        if items.is_empty() || out.len() as u64 >= total {
            return Ok(out);
        }
    }
}

/// What the chain holds for one value: the contract's registry, its recheck fact, its open
/// markets and every bettor of every market of it (open or not: a bettor of a void market is
/// still left out).
struct Value {
    registry: [u8; 20],
    recheck_fact: Hash32,
    open: Vec<OpenMarket>,
    all: Vec<u64>,
    bettors: Vec<[u8; 20]>,
}

/// The value's lists at `block` (CL7-R2: the plan pins them to the head it decides by, so no bet
/// lands between the bettors it reads and the close it checks), or at the head with `None`.
fn read_value(
    chain: &CastChain,
    corrections: &[u8; 20],
    original: &FeedTemplate,
    recheck: &FeedTemplate,
    subject: &Subject,
    block: Option<u64>,
) -> Result<Value, String> {
    // The pair must be this recheck (THREATS CL7-8): the contract's recheck metric for the
    // original, and the registry's template for it.
    let pair = call(
        chain,
        corrections,
        "pairs(bytes32)(bytes32,uint128)",
        &[h(&metric_key(&original.metric))],
    )?;
    if pair.first().and_then(Json::as_str).map(str::to_lowercase)
        != Some(h(&metric_key(&recheck.metric)))
    {
        return Err(format!(
            "{} is not paired with {} on this contract",
            original.metric.0, recheck.metric.0
        ));
    }
    let registry = parse_addr(
        call(chain, corrections, "registry()(address)", &[])?
            .first()
            .and_then(Json::as_str)
            .ok_or("registry(): no address")?,
    )?;
    let ret = chain_eth_call(chain, &registry, &registry_metric_calldata(recheck))?;
    check_registered(recheck, &ret).map_err(|e| format!("{}: {e}", recheck.metric.0))?;
    let recheck_fact = fact_of(recheck, subject);
    let ids = paged(
        chain,
        corrections,
        "marketsOfRecheck(bytes32,uint256,uint256)(uint256[],uint256)",
        &h(&recheck_fact),
        "marketsOfRecheck",
        block,
    )?;
    let mut value = Value {
        registry,
        recheck_fact,
        open: Vec::new(),
        all: Vec::new(),
        bettors: Vec::new(),
    };
    for id in ids {
        let id = word(&id, "market id")?;
        let m = call_at(chain, corrections, MARKET_SIG, &[id.to_string()], block)?;
        let m = m
            .first()
            .and_then(Json::as_array)
            .ok_or("market(): no struct")?;
        let field = |i: usize, what: &str| m.get(i).ok_or(format!("market {id}: no {what}"));
        if field(3, "recheckFact")?.as_str().map(str::to_lowercase) != Some(h(&recheck_fact)) {
            return Err(format!("market {id} names another recheck fact"));
        }
        value.all.push(id);
        if num(field(8, "state")?, "state")? == OPEN {
            value.open.push(OpenMarket {
                id,
                close_block: word(field(6, "closeBlock")?, "closeBlock")?,
                resolve_block: word(field(7, "resolveBlock")?, "resolveBlock")?,
            });
        }
        for b in paged(
            chain,
            corrections,
            "bettorsOf(uint256,uint256,uint256)(address[],uint256)",
            &id.to_string(),
            "bettorsOf",
            block,
        )? {
            value
                .bettors
                .push(parse_addr(b.as_str().ok_or("bettorsOf: not an address")?)?);
        }
    }
    Ok(value)
}

fn chain_eth_call(chain: &CastChain, to: &[u8; 20], data: &[u8]) -> Result<Vec<u8>, String> {
    let out = chain.run(&["call".into(), ax(to), format!("0x{}", hex(data))])?;
    let s = out.trim().trim_start_matches("0x");
    let mut b = vec![0u8; s.len() / 2];
    crate::live::hex_to(s, &mut b).map_err(|e| format!("eth_call: {e}"))?;
    Ok(b)
}

/// The original round's observers for `subject`, from its results store checked under the
/// original template (as the server checks a store) at `now`, and only when that round is the
/// one the registry recorded (`observationOf(original).result`): a store of another round (an
/// old copy, another deployment) would leave its observers in the recheck pool (CL7-3).
fn original_observers(
    chain: &CastChain,
    registry: &[u8; 20],
    original: &FeedTemplate,
    store: &str,
    subject: &Subject,
    now: u64,
) -> Result<Vec<[u8; 20]>, String> {
    let pins = TemplatePins::from([(original.metric.0.clone(), original.id())]);
    let feed = ObservationStore::from_json(store, now, &pins).map_err(|e| e.to_string())?;
    let r = feed
        .record(subject_text(subject), None, None)
        .ok_or(format!("no settled round for {}", subject_text(subject)))?;
    let o = call(
        chain,
        registry,
        "observationOf(bytes32,bytes32,uint64)((bytes32,bytes32,bytes32,uint64,bytes32,bytes32,uint64))",
        &[
            h(&metric_key(&original.metric)),
            h(&subject_key(subject)),
            r.checked.spec.window_end.to_string(),
        ],
    )?;
    let recorded = o
        .first()
        .and_then(Json::as_array)
        .and_then(|o| o.get(5))
        .and_then(Json::as_str)
        .map(str::to_lowercase)
        .ok_or("observationOf: no result")?;
    if recorded != h(&r.checked.result) {
        return Err(format!(
            "the original store's round of {} is not the one the registry recorded (result {recorded}, the store's {}): its observers cannot be left out",
            subject_text(subject),
            h(&r.checked.result)
        ));
    }
    Ok(r.transcript.observers.iter().map(|o| o.address).collect())
}

/// The recheck pool: `full` without the bettors and the original observers, a valid snapshot;
/// with the size of `full`.
fn preview(
    full: &[Observer],
    bettors: &[[u8; 20]],
    originals: &[[u8; 20]],
) -> Result<(Vec<Observer>, usize), String> {
    let pool = recheck_pool(full, bettors, originals);
    check_pool(&pool).map_err(|e| format!("the recheck pool: {e}"))?;
    Ok((pool, full.len()))
}

/// A pool snapshot as [`parse_pool`] reads it: `[{"address": "0x…", "bond": 1000}, …]`.
pub fn pool_json(pool: &[Observer]) -> Result<Json, String> {
    pool.iter()
        .map(|o| {
            let bond = u64::try_from(o.bond).map_err(|_| "a bond above u64".to_string())?;
            Ok(json!({"address": ax(&o.address), "bond": bond}))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Json::Array)
}

fn margin_of(args: &[String]) -> Result<u64, String> {
    Ok(match flag(args, "--margin-blocks")? {
        Some(m) => m.parse().map_err(|e| format!("--margin-blocks: {e}"))?,
        None => DEFAULT_MARGIN_BLOCKS,
    })
}

fn recheck_args(args: &[String]) -> Result<(u16, u16), String> {
    let v = need(args, "--recheck")?;
    let (a, s) = v.split_once(':').ok_or("--recheck AFTER_DAYS:SPAN_DAYS")?;
    Ok((
        a.parse().map_err(|e| format!("--recheck after: {e}"))?,
        s.parse().map_err(|e| format!("--recheck span: {e}"))?,
    ))
}

fn chain_of(args: &[String]) -> Result<CastChain, String> {
    let mut a = vec![
        "--rpc".to_string(),
        need(args, "--rpc")?,
        "--registry".into(),
        "0x0000000000000000000000000000000000000001".into(),
        "--coordinator".into(),
        "0x0000000000000000000000000000000000000001".into(),
    ];
    if let Some(c) = flag(args, "--cast")? {
        a.extend(["--cast".into(), c]);
    }
    CastChain::from_args(&a)?.ok_or_else(|| "--rpc URL is required".into())
}

/// `recheck-plan` (see the module docs). `original` is the value's own source.
pub fn plan(original: &dyn Source, args: &[String]) -> Result<Json, String> {
    let (after, span) = recheck_args(args)?;
    let t = original.template();
    let rt = recheck_of(&t, after, span).map_err(|e| e.to_string())?;
    let date = need(args, "--record-date")?;
    let subject = subject_of(&t, &date)?;
    let chain = chain_of(args)?;
    let corrections = parse_addr(&need(args, "--corrections")?)?;
    let margin = margin_of(args)?;
    let (head, now) = block_now(&chain)?;
    let v = read_value(&chain, &corrections, &t, &rt, &subject, Some(head))?;
    let mut out = json!({
        "metric": rt.metric.0,
        "template": h(&rt.id()),
        "subject": subject_text(&subject),
        "recheckFact": h(&v.recheck_fact),
        "markets": v.all,
        "bettors": v.bettors.len(),
        "headBlock": head,
    });
    let Some(served) = serve(&v.open, margin) else {
        out["open"] = json!(v.open.iter().map(|m| m.id).collect::<Vec<_>>());
        if v.open.is_empty() {
            out["state"] = json!("none");
        } else {
            out["state"] = json!("void-only");
            out["reason"] = json!("no open market closes the margin before its deadline");
        }
        return Ok(out);
    };
    out["served"] = json!(served.markets);
    out["left"] = json!(served.left);
    out["afterBlock"] = json!(served.after_block);
    out["deadlineBlock"] = json!(served.deadline_block);
    if head <= served.after_block {
        out["state"] = json!("wait");
        // A preview for the operator's pre-flight: bettors may still join until the close.
        if flag(args, "--pool")?.is_some() {
            let (pool, full) = pool_of(&chain, &t, &subject, &v, args, now)?;
            write_pool(args, &pool, &mut out)?;
            out["provisional"] = json!(true);
            out["pool"] = json!(pool.len());
            out["excluded"] = json!(full - pool.len());
            out["minObservers"] = json!(rt.min_observers);
        }
        return Ok(out);
    }
    let close_time = block_time(&chain, served.after_block)?;
    let Some(start) = recheck_window_start(&rt, &subject, close_time).map_err(|e| e.to_string())?
    else {
        out["state"] = json!("void-only");
        out["reason"] = json!("no recheck day is left after the close (THREATS CL7-13)");
        return Ok(out);
    };
    let (pool, full) = pool_of(&chain, &t, &subject, &v, args, now)?;
    if pool.len() < usize::from(rt.min_observers) {
        return Err(format!(
            "the recheck pool has {} members without bettors and original observers; the round needs {}",
            pool.len(),
            rt.min_observers
        ));
    }
    write_pool(args, &pool, &mut out)?;
    out["state"] = json!("run");
    out["windowStart"] = json!(start);
    out["closeTime"] = json!(close_time);
    out["pool"] = json!(pool.len());
    out["excluded"] = json!(full - pool.len());
    Ok(out)
}

/// The pool to `--out-pool`, when given (named in the plan as `outPool`).
fn write_pool(args: &[String], pool: &[Observer], out: &mut Json) -> Result<(), String> {
    if let Some(p) = flag(args, "--out-pool")? {
        std::fs::write(
            &p,
            serde_json::to_string_pretty(&pool_json(pool)?).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| format!("{p}: {e}"))?;
        out["outPool"] = json!(p);
    }
    Ok(())
}

/// The recheck pool from `--pool` without the value's bettors and the original round's observers
/// (from `--original-store`), checked as a snapshot; with the size of `--pool`.
fn pool_of(
    chain: &CastChain,
    t: &FeedTemplate,
    subject: &Subject,
    v: &Value,
    args: &[String],
    now: u64,
) -> Result<(Vec<Observer>, usize), String> {
    let path = need(args, "--original-store")?;
    let store = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let originals = original_observers(chain, &v.registry, t, &store, subject, now)?;
    let pool_path = need(args, "--pool")?;
    let full =
        parse_pool(&std::fs::read_to_string(&pool_path).map_err(|e| format!("{pool_path}: {e}"))?)?;
    preview(&full, &v.bettors, &originals)
}

/// Who sends to `OnyxCorrections`: a key from a key file (kept in the process), or on a local
/// anvil an unlocked account (`--from ADDR`; anvil refuses keys, `KeySigner::new`).
enum Sender {
    Key(KeySigner),
    Unlocked([u8; 20], Box<CastChain>),
}

impl Sender {
    fn address(&self) -> [u8; 20] {
        match self {
            Sender::Key(k) => k.address(),
            Sender::Unlocked(a, _) => *a,
        }
    }

    /// Send `sig(args)` to the corrections contract; or with `dry`, only estimate its gas
    /// (`eth_estimateGas` from this sender: a call that would revert is an error). `Ok` holds
    /// the receipt's block number, or the estimate.
    fn send_or_estimate(
        &self,
        chain: &CastChain,
        corrections: &[u8; 20],
        sig: &str,
        args: &[String],
        dry: bool,
    ) -> Result<u64, String> {
        if dry {
            let mut a = vec![
                "estimate".to_string(),
                "--from".into(),
                ax(&self.address()),
                ax(corrections),
                sig.into(),
            ];
            a.extend(args.iter().cloned());
            let gas = chain.run(&a)?;
            return u64::try_from(num(&Json::String(gas), "gas")?).map_err(|e| e.to_string());
        }
        match self {
            Sender::Key(k) => send(chain, k, corrections, sig, args),
            Sender::Unlocked(from, to) => to.send(from, sig, args),
        }
    }
}

/// A `bool` as `cast call --json` decodes it (`true`, or the text `"true"`).
fn is_true(v: &Json) -> bool {
    v.as_bool() == Some(true) || v.as_str() == Some("true")
}

/// Every value of a repeatable flag, in order.
fn all(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

/// An unlocked anvil account that sends to `corrections` (`CastChain::send` sends to its
/// registry: here the corrections contract).
fn unlocked(args: &[String], corrections: &[u8; 20], from: &str) -> Result<Sender, String> {
    let mut a = vec![
        "--rpc".to_string(),
        need(args, "--rpc")?,
        "--registry".into(),
        ax(corrections),
        "--coordinator".into(),
        from.to_owned(),
    ];
    if let Some(c) = flag(args, "--cast")? {
        a.extend(["--cast".into(), c]);
    }
    let to = CastChain::from_args(&a)?.ok_or("--rpc URL is required")?;
    Ok(Sender::Unlocked(parse_addr(from)?, Box::new(to)))
}

/// The senders `key_flag` names (key files) on a live chain, or `from_flag` (unlocked accounts)
/// on a local anvil.
fn senders(
    chain: &CastChain,
    corrections: &[u8; 20],
    args: &[String],
    key_flag: &str,
    from_flag: &str,
) -> Result<Vec<Sender>, String> {
    if chain.chain_id()? != ANVIL_CHAIN_ID {
        return all(args, key_flag)
            .iter()
            .map(|f| signer_of(chain, f).map(Sender::Key))
            .collect();
    }
    all(args, from_flag)
        .iter()
        .map(|a| unlocked(args, corrections, a))
        .collect()
}

fn sender(chain: &CastChain, corrections: &[u8; 20], args: &[String]) -> Result<Sender, String> {
    let flag = if chain.chain_id()? == ANVIL_CHAIN_ID {
        "--from"
    } else {
        "--key-file"
    };
    let mut s = senders(chain, corrections, args, "--key-file", "--from")?;
    match s.len() {
        1 => Ok(s.remove(0)),
        0 => Err(format!("{flag} is required")),
        _ => Err(format!("one {flag} sends resolve")),
    }
}

/// `recheck-resolve` (see the module docs): every open market of the value, resolved from the
/// recheck round in `--store` (its template pinned, the round re-verified, its fact the value's
/// recheck fact). Only the markets the round serves are sent ([`serve`] with the plan's
/// `--margin-blocks`); one left to void is reported as `leftToVoid`. The contract decides each
/// one (a recheck drawn before a market's close voids it there, CL7-2); a market it refuses is
/// reported and the rest go on.
pub fn resolve(original: &dyn Source, args: &[String]) -> Result<Json, String> {
    let (after, span) = recheck_args(args)?;
    let t = original.template();
    let rt = recheck_of(&t, after, span).map_err(|e| e.to_string())?;
    let date = need(args, "--record-date")?;
    let subject = subject_of(&t, &date)?;
    let chain = chain_of(args)?;
    let corrections = parse_addr(&need(args, "--corrections")?)?;
    let v = read_value(&chain, &corrections, &t, &rt, &subject, None)?;
    // As the plan: with no market served (none closes the margin before the earliest deadline),
    // every open market is left to void.
    let left = match serve(&v.open, margin_of(args)?) {
        Some(s) => s.left,
        None => v.open.iter().map(|m| m.id).collect(),
    };
    let path = need(args, "--store")?;
    let store = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let (_, now) = block_now(&chain)?;
    let pins = TemplatePins::from([(rt.metric.0.clone(), rt.id())]);
    let feed =
        ObservationStore::from_json(&store, now, &pins).map_err(|e| format!("{path}: {e}"))?;
    let r = feed
        .record(subject_text(&subject), None, None)
        .ok_or(format!(
            "{path}: no settled recheck round for {}",
            subject_text(&subject)
        ))?;
    if r.checked.registry_fact != v.recheck_fact {
        return Err(format!("{path}: the round is for another fact"));
    }
    let Answer::Number(value) = r.transcript.answer else {
        return Err(format!("{path}: the recheck settled no number"));
    };
    let who = sender(&chain, &corrections, args)?;
    let dry = args.iter().any(|a| a == "--dry-run");
    let mut done = Vec::new();
    for m in v.open.iter().filter(|m| !left.contains(&m.id)) {
        let a = [m.id.to_string(), h(&r.checked.round_id), value.to_string()];
        let sig = "resolve(uint256,bytes32,int128)";
        let res = who.send_or_estimate(&chain, &corrections, sig, &a, dry);
        done.push(match (res, dry) {
            (Ok(gas), true) => json!({"market": m.id, "resolved": false, "gas": gas}),
            (Ok(_), false) => json!({"market": m.id, "resolved": true}),
            (Err(e), _) => json!({"market": m.id, "resolved": false, "error": e}),
        });
    }
    Ok(json!({
        "metric": rt.metric.0,
        "subject": subject_text(&subject),
        "round": h(&r.checked.round_id),
        "value": value.to_string(),
        "from": ax(&who.address()),
        "dryRun": dry,
        "markets": done,
        "leftToVoid": left,
    }))
}

/// What `recheck-payout` does for one key in one market.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PayStep {
    /// The market is still open: nothing is owed yet.
    Open,
    /// `withdraw`: a winner, or any bettor of a void market.
    Withdraw,
    /// Paid by an earlier run: nothing is sent.
    PaidBefore,
    /// Nothing owed and never paid: no winning bet.
    Nothing,
}

/// The step for a key of a market in `state` that the contract `owed` and had `paid` before.
fn pay_step(state: u128, owed: u128, paid: bool) -> PayStep {
    if state == OPEN {
        PayStep::Open
    } else if owed > 0 {
        PayStep::Withdraw
    } else if paid {
        PayStep::PaidBefore
    } else {
        PayStep::Nothing
    }
}

/// `recheck-payout` (see the module docs): the withdrawals of the keys given from every settled
/// market of the value, then the treasury's payment. Nothing is sent for what is done already
/// (`owed` is 0 once paid), so the command can run again; a send that fails is reported and the
/// rest go on.
pub fn payout(original: &dyn Source, args: &[String]) -> Result<Json, String> {
    let (after, span) = recheck_args(args)?;
    let t = original.template();
    let rt = recheck_of(&t, after, span).map_err(|e| e.to_string())?;
    let date = need(args, "--record-date")?;
    let subject = subject_of(&t, &date)?;
    let chain = chain_of(args)?;
    let corrections = parse_addr(&need(args, "--corrections")?)?;
    let dry = args.iter().any(|a| a == "--dry-run");
    let v = read_value(&chain, &corrections, &t, &rt, &subject, None)?;
    let keys = senders(&chain, &corrections, args, "--key-file", "--from")?;
    let treasury = senders(
        &chain,
        &corrections,
        args,
        "--treasury-key-file",
        "--treasury-from",
    )?;
    if treasury.len() > 1 {
        return Err("one --treasury-key-file sends payTreasury".into());
    }
    let word_of = |sig: &str, a: &[String]| -> Result<u128, String> {
        num(
            call(&chain, &corrections, sig, a)?
                .first()
                .ok_or(format!("{sig}: no answer"))?,
            sig,
        )
    };
    let mut markets = Vec::new();
    let mut paid = Vec::new();
    for id in &v.all {
        let m = call(&chain, &corrections, MARKET_SIG, &[id.to_string()])?;
        let m = m
            .first()
            .and_then(Json::as_array)
            .ok_or("market(): no struct")?;
        let state = num(m.get(8).ok_or("market: no state")?, "state")?;
        let winner = num(m.get(9).ok_or("market: no winner")?, "winner")?;
        markets.push(json!({"market": id, "state": state_name(state), "winner": winner}));
        if state == OPEN {
            continue;
        }
        for k in &keys {
            let who = ax(&k.address());
            let a = [id.to_string(), who.clone()];
            let owed = word_of("owed(uint256,address)(uint256)", &a)?;
            let paid_before = call(&chain, &corrections, "paid(uint256,address)(bool)", &a)?
                .first()
                .is_some_and(is_true);
            match pay_step(state, owed, paid_before) {
                PayStep::Withdraw => {}
                PayStep::PaidBefore => {
                    paid.push(json!({"market": id, "who": who, "paidBefore": true}));
                    continue;
                }
                PayStep::Open | PayStep::Nothing => continue,
            }
            let res = k.send_or_estimate(
                &chain,
                &corrections,
                "withdraw(uint256)",
                &[id.to_string()],
                dry,
            );
            paid.push(match (res, dry) {
                (Ok(gas), true) => {
                    json!({"market": id, "who": who, "owed": owed.to_string(), "paid": false, "gas": gas})
                }
                (Ok(_), false) => {
                    json!({"market": id, "who": who, "owed": owed.to_string(), "paid": true})
                }
                (Err(e), _) => {
                    json!({"market": id, "who": who, "owed": owed.to_string(), "paid": false, "error": e})
                }
            });
        }
    }
    let owed = word_of("treasuryOwed()(uint256)", &[])?;
    let mut tr = json!({"owed": owed.to_string(), "paid": false});
    if let Some(s) = treasury.first() {
        tr["from"] = json!(ax(&s.address()));
        if owed > 0 {
            match s.send_or_estimate(&chain, &corrections, "payTreasury()", &[], dry) {
                Ok(gas) if dry => tr["gas"] = json!(gas),
                Ok(_) => tr["paid"] = json!(true),
                Err(e) => tr["error"] = json!(e),
            }
        }
    }
    Ok(json!({
        "metric": rt.metric.0,
        "subject": subject_text(&subject),
        "dryRun": dry,
        "markets": markets,
        "withdrawals": paid,
        "treasury": tr,
    }))
}

/// Run one recheck runner command for the value's own source `original`.
pub fn run(original: &dyn Source, args: &[String]) -> Result<(), String> {
    let out = match args.get(1).map(String::as_str).unwrap_or_default() {
        "recheck-plan" => plan(original, args)?,
        "recheck-resolve" => resolve(original, args)?,
        "recheck-payout" => payout(original, args)?,
        _ => return Err("usage: recheck-plan|recheck-resolve|recheck-payout --recheck A:S --record-date D --rpc URL --corrections ADDR ...".into()),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The subject a correction market names is the original's, for a recheck too; only a
    /// date names one.
    #[test]
    fn a_date_names_the_originals_subject() {
        let src = crate::manifest::ManifestSource::from_file(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../sources/us.treasury.tga_total_deposits/manifest.json"),
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        )
        .expect("manifest");
        let t = src.template();
        let rt = recheck_of(&t, 7, 7).expect("recheck");
        let s = subject_of(&t, "2026-10-27").expect("dated");
        assert_eq!(subject_of(&rt, "2026-10-27"), Ok(s.clone()));
        assert_eq!(
            subject_text(&s),
            format!(
                "{}:2026-10-27",
                subject_text(&s).split(':').next().unwrap_or_default()
            )
        );
        assert!(subject_of(&t, "2026-13-40").is_err());
        assert!(subject_of(&t, "yesterday").is_err());
        assert_ne!(
            fact_of(&t, &s),
            fact_of(&rt, &s),
            "the recheck has its own fact"
        );
    }

    /// A repeatable flag gives every value in order; the market states have their names.
    #[test]
    fn repeated_flags_and_state_names() {
        let a: Vec<String> = [
            "x",
            "--key-file",
            "a",
            "--from",
            "0x01",
            "--key-file",
            "b",
            "--key-file",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(all(&a, "--key-file"), vec!["a", "b"]);
        assert_eq!(all(&a, "--from"), vec!["0x01"]);
        assert!(all(&a, "--treasury-key-file").is_empty());
        assert!(is_true(&json!(true)) && is_true(&json!("true")));
        assert!(!is_true(&json!(false)) && !is_true(&json!("false")) && !is_true(&json!(1)));
        assert_eq!(
            [0, 1, 2, 3, 4].map(state_name),
            ["unknown", "open", "resolved", "void", "unknown"]
        );
    }

    /// recheck-payout's skip logic: nothing for an open market, a withdrawal for whatever is
    /// owed (a winner, or any bettor of a void market), nothing sent again once paid.
    #[test]
    fn payout_withdraws_what_is_owed_once() {
        assert_eq!(pay_step(OPEN, 10, false), PayStep::Open);
        assert_eq!(pay_step(2, 19, false), PayStep::Withdraw);
        assert_eq!(
            pay_step(3, 10, false),
            PayStep::Withdraw,
            "a void market refunds"
        );
        assert_eq!(pay_step(2, 0, true), PayStep::PaidBefore);
        assert_eq!(pay_step(2, 0, false), PayStep::Nothing, "a losing bet");
    }

    /// The recheck pool (the plan's, and its preview while trading is open) leaves out every
    /// bettor and original observer; a pool left empty is refused.
    #[test]
    fn the_preview_pool_leaves_out_bettors_and_original_observers() {
        let full: Vec<Observer> = (1..=16u8)
            .map(|n| Observer {
                address: [n; 20],
                bond: 1_000,
            })
            .collect();
        let originals: Vec<[u8; 20]> = (12..=16u8).map(|n| [n; 20]).collect();
        let bettors = vec![[3; 20], [99; 20], [3; 20]];
        let (pool, n) = preview(&full, &bettors, &originals).expect("pool");
        assert_eq!((pool.len(), n), (10, 16));
        assert!(pool
            .iter()
            .all(|o| o.address != [3; 20] && o.address[0] < 12));
        let all: Vec<[u8; 20]> = full.iter().map(|o| o.address).collect();
        assert!(preview(&full, &all, &[]).is_err(), "an empty pool");
    }

    /// The plan's pool file is read back by the coordinator as the same snapshot.
    #[test]
    fn the_plan_pool_file_reads_back() {
        let pool = vec![
            Observer {
                address: [1; 20],
                bond: 1_000,
            },
            Observer {
                address: [2; 20],
                bond: 5,
            },
        ];
        let text = serde_json::to_string(&pool_json(&pool).expect("json")).expect("text");
        assert_eq!(parse_pool(&text), Ok(pool));
        let big = [Observer {
            address: [1; 20],
            bond: u128::from(u64::MAX) + 1,
        }];
        assert!(pool_json(&big).is_err());
    }
}
