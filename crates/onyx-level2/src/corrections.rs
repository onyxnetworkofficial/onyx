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
//! - `recheck-resolve --record-date D --rpc URL --corrections ADDR --store FILE --key-file F`:
//!   `resolve` every open market of the value from the recheck's results store (anyone may). On a
//!   local anvil it sends from an unlocked account instead (`--from ADDR`: no key there).

use crate::live::{flag, hex, need, KeySigner};
use crate::markets::{ax, block_now, block_time, call, num, send, signer_of};
use crate::{parse_pool, Source};
use onyx_api::observations::{
    check_registered, registry_metric_calldata, subject_text, ObservationStore, TemplatePins,
};
use onyx_coordinator::cast_chain::{parse_addr, CastChain, ANVIL_CHAIN_ID};
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
/// given: 7,200 (24 minutes at Base Sepolia's 0.2 s blocks), at most the deployment's
/// `minRecheckBlocks` in practice.
pub const DEFAULT_MARGIN_BLOCKS: u64 = 7_200;

/// `Market.state` of an open market.
const OPEN: u128 = 1;

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

/// One page-wise list read: `(items, total)` pages of `sig` until `total` items are read.
fn paged(
    chain: &CastChain,
    to: &[u8; 20],
    sig: &str,
    key: &str,
    what: &str,
) -> Result<Vec<Json>, String> {
    let mut out = Vec::new();
    loop {
        let r = call(
            chain,
            to,
            sig,
            &[key.to_owned(), out.len().to_string(), PAGE.to_string()],
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

/// What the chain holds for one value: its recheck fact, its open markets and every bettor of
/// every market of it (open or not: a bettor of a void market is still left out).
struct Value {
    recheck_fact: Hash32,
    open: Vec<OpenMarket>,
    all: Vec<u64>,
    bettors: Vec<[u8; 20]>,
}

fn read_value(
    chain: &CastChain,
    corrections: &[u8; 20],
    original: &FeedTemplate,
    recheck: &FeedTemplate,
    subject: &Subject,
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
    )?;
    let mut value = Value {
        recheck_fact,
        open: Vec::new(),
        all: Vec::new(),
        bettors: Vec::new(),
    };
    for id in ids {
        let id = word(&id, "market id")?;
        let m = call(chain, corrections, MARKET_SIG, &[id.to_string()])?;
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
/// original template (as the server checks a store) at `now`.
fn original_observers(
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
    Ok(r.transcript.observers.iter().map(|o| o.address).collect())
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
    let margin: u64 = match flag(args, "--margin-blocks")? {
        Some(m) => m.parse().map_err(|e| format!("--margin-blocks: {e}"))?,
        None => DEFAULT_MARGIN_BLOCKS,
    };
    let v = read_value(&chain, &corrections, &t, &rt, &subject)?;
    let (head, now) = block_now(&chain)?;
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
        return Ok(out);
    }
    let close_time = block_time(&chain, served.after_block)?;
    let Some(start) = recheck_window_start(&rt, &subject, close_time).map_err(|e| e.to_string())?
    else {
        out["state"] = json!("void-only");
        out["reason"] = json!("no recheck day is left after the close (THREATS CL7-13)");
        return Ok(out);
    };
    let path = need(args, "--original-store")?;
    let store = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let originals = original_observers(&t, &store, &subject, now)?;
    let pool_path = need(args, "--pool")?;
    let full =
        parse_pool(&std::fs::read_to_string(&pool_path).map_err(|e| format!("{pool_path}: {e}"))?)?;
    let pool = recheck_pool(&full, &v.bettors, &originals);
    check_pool(&pool).map_err(|e| format!("the recheck pool: {e}"))?;
    if pool.len() < usize::from(rt.min_observers) {
        return Err(format!(
            "the recheck pool has {} members without bettors and original observers; the round needs {}",
            pool.len(),
            rt.min_observers
        ));
    }
    if let Some(p) = flag(args, "--out-pool")? {
        std::fs::write(
            &p,
            serde_json::to_string_pretty(&pool_json(&pool)?).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| format!("{p}: {e}"))?;
        out["outPool"] = json!(p);
    }
    out["state"] = json!("run");
    out["windowStart"] = json!(start);
    out["closeTime"] = json!(close_time);
    out["pool"] = json!(pool.len());
    out["excluded"] = json!(full.len() - pool.len());
    Ok(out)
}

/// Who sends `resolve`: the key in `--key-file` (kept in the process), or on a local anvil an
/// unlocked account (`--from ADDR`; anvil refuses keys, `KeySigner::new`).
enum Sender {
    Key(KeySigner),
    Unlocked([u8; 20], Box<CastChain>),
}

fn sender(chain: &CastChain, corrections: &[u8; 20], args: &[String]) -> Result<Sender, String> {
    if chain.chain_id()? != ANVIL_CHAIN_ID {
        return Ok(Sender::Key(signer_of(chain, &need(args, "--key-file")?)?));
    }
    let from = need(args, "--from")?;
    // `CastChain::send` sends to its registry: here the corrections contract.
    let mut a = vec![
        "--rpc".to_string(),
        need(args, "--rpc")?,
        "--registry".into(),
        ax(corrections),
        "--coordinator".into(),
        from.clone(),
    ];
    if let Some(c) = flag(args, "--cast")? {
        a.extend(["--cast".into(), c]);
    }
    let to = CastChain::from_args(&a)?.ok_or("--rpc URL is required")?;
    Ok(Sender::Unlocked(parse_addr(&from)?, Box::new(to)))
}

/// `recheck-resolve` (see the module docs): every open market of the value, resolved from the
/// recheck round in `--store` (its template pinned, the round re-verified, its fact the value's
/// recheck fact). The contract decides each one (a recheck drawn before a market's close voids
/// it there, CL7-2); a market it refuses is reported and the rest go on.
pub fn resolve(original: &dyn Source, args: &[String]) -> Result<Json, String> {
    let (after, span) = recheck_args(args)?;
    let t = original.template();
    let rt = recheck_of(&t, after, span).map_err(|e| e.to_string())?;
    let date = need(args, "--record-date")?;
    let subject = subject_of(&t, &date)?;
    let chain = chain_of(args)?;
    let corrections = parse_addr(&need(args, "--corrections")?)?;
    let v = read_value(&chain, &corrections, &t, &rt, &subject)?;
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
    let mut done = Vec::new();
    for m in &v.open {
        let a = [m.id.to_string(), h(&r.checked.round_id), value.to_string()];
        let sig = "resolve(uint256,bytes32,int128)";
        let res = match &who {
            Sender::Key(k) => send(&chain, k, &corrections, sig, &a),
            Sender::Unlocked(from, to) => to.send(from, sig, &a),
        };
        done.push(match res {
            Ok(_) => json!({"market": m.id, "resolved": true}),
            Err(e) => json!({"market": m.id, "resolved": false, "error": e}),
        });
    }
    Ok(json!({
        "metric": rt.metric.0,
        "subject": subject_text(&subject),
        "round": h(&r.checked.round_id),
        "value": value.to_string(),
        "markets": done,
    }))
}

/// Run one recheck runner command for the value's own source `original`.
pub fn run(original: &dyn Source, args: &[String]) -> Result<(), String> {
    let out = match args.get(1).map(String::as_str).unwrap_or_default() {
        "recheck-plan" => plan(original, args)?,
        "recheck-resolve" => resolve(original, args)?,
        _ => return Err("usage: recheck-plan|recheck-resolve --recheck A:S --record-date D --rpc URL --corrections ADDR ...".into()),
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
