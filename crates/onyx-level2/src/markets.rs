//! Futures markets on a dated level-2 fact, live on Base Sepolia (NORTH-STAR
//! NS11, ADR-0042 amendment 2): open a market on a value no round can observe
//! yet, take tONYX test bets, and settle it from the value observers record
//! in `OnyxRegistry`, proved to `OnyxObservedFacts` by its round id.
//!
//! The deployment (`scripts/markets-sepolia.sh deploy`) is a JSON file
//! (`--markets FILE`) naming the contracts; this module adds each market it
//! opens and each bettor to it. The admin key (the deployer's: it mints
//! tONYX, owns the futures and lists facts) comes from [`ADMIN_KEY_ENV`];
//! each bettor signs with its own `--key-file`. Keys stay in the process:
//! never printed, never on a `cast` command line ([`KeySigner`]).
//!
//! Commands (`examples/market.rs` of a dated source):
//! - `open --record-date D --edges E1[,E2...] --close T --observable T
//!   --resolve-by T`: refused unless no round may observe D before trading
//!   closes and one may by `--observable`.
//! - `bet --market N --key-file F --bucket B --amount A` (whole tONYX).
//! - `settle --market N --store FILE [--key-file F]...`: prove the value from
//!   the round's results store, resolve, pay each listed bettor.
//! - `status [--out FILE]`: every market as JSON (the website's snapshot).
//! - `underwrite --key-file F --amount A` (whole tONYX): fund the pool of the
//!   deployment's `OnyxParametric` (`--markets` names `insurance.json`), so
//!   its products have cover for sale. Insiders and observers are refused
//!   before any gas grant or mint (R-PI-4).

use crate::live::{flag, hex, hex_to, need, KeySigner};
use crate::Source;
use onyx_api::observations::subject_text;
use onyx_api::observations::{ObservationStore, TemplatePins};
use onyx_coordinator::cast_chain::{parse_addr, CastChain, TxSigner, ANVIL_CHAIN_ID};
use onyx_coordinator::keyfile::read_key_file;
use onyx_coordinator::observe::{Clock, SystemClock};
use onyx_core::onchain::{metric_key, registry_fact_key, subject_key, MetricKind};
use onyx_core::Answer;
use onyx_core::Hash32;
use serde_json::{json, Value as Json};
use std::path::Path;

/// The environment variable the admin's key is read from (hex, 32 bytes).
pub const ADMIN_KEY_ENV: &str = "ONYX_MARKETS_ADMIN_KEY";

/// Test ETH a new bettor gets for gas, in wei (0.00005 ETH: hundreds of
/// sends at Base Sepolia's fee).
pub const GAS_GRANT_WEI: u128 = 50_000_000_000_000;

/// A bettor below this balance gets [`GAS_GRANT_WEI`] first.
pub const GAS_FLOOR_WEI: u128 = 20_000_000_000_000;

/// One tONYX in its smallest unit (18 decimals).
pub const TONYX: u128 = 1_000_000_000_000_000_000;

/// `OnyxFutures.market(id)` for `cast call` (the `Market` struct).
const MARKET_SIG: &str = "market(uint256)((bytes32,uint64,uint64,uint64,uint8,uint8,uint32,uint32,uint32,uint128,uint128,uint128,uint128,int256,int256[],uint128[],uint32[]))";

/// Trading closes at least this long before the source may show the value
/// ([`Source::earliest_public`], THREATS NL6-10).
pub const CLOSE_MARGIN_SECS: u64 = 3_600;

/// The latest trading close for a market on `date`: the listing's `closeBy`
/// (THREATS NL6-10).
pub fn close_by(src: &dyn Source, date: &str) -> Result<u64, String> {
    src.earliest_public(date)?
        .checked_sub(CLOSE_MARGIN_SECS)
        .ok_or(format!("{date}: no close before the source publishes"))
}

/// Refused unless trading closes in the future and no later than `close_by`.
pub fn check_close(close: u64, close_by: u64, now: u64) -> Result<(), String> {
    if close <= now {
        return Err("trading must close in the future".into());
    }
    if close > close_by {
        return Err(format!(
            "trading must close by {close_by}, before the source may show the value (THREATS NL6-10)"
        ));
    }
    Ok(())
}

/// The block expected at time `at`, from the chain's recent rate: the first
/// block a recording may come from is the one at `observableAt`, so an early
/// recording never proves (THREATS NL6-11).
pub fn block_at(head: u64, now: u64, at: u64, per_block_ms: u64) -> u64 {
    head + at.saturating_sub(now) * 1_000 / per_block_ms.max(1)
}

/// Refused unless the bet can succeed, checked before any gas grant or mint
/// (THREATS NL6-14): the key may bet (`mayBet`), the market is open
/// (`state` 1) and trading, and the bucket exists.
pub fn bet_precheck(
    may_bet: bool,
    state: u128,
    trading_close: u64,
    now: u64,
    bucket: u8,
    buckets: usize,
) -> Result<(), String> {
    if !may_bet {
        return Err(
            "this key may not bet (an insider, observer, owner or treasury, THREATS NL6-12)".into(),
        );
    }
    if state != 1 || now >= trading_close {
        return Err("the market is not open for bets".into());
    }
    if usize::from(bucket) >= buckets {
        return Err(format!("bucket {bucket}: the market has {buckets}"));
    }
    Ok(())
}

/// Refused unless the deposit can succeed, checked before any gas grant or
/// mint (R-PI-4, as NL6-14 for bets): the key may take part and the amount
/// is not zero.
pub fn underwrite_precheck(may_take_part: bool, whole: u128) -> Result<(), String> {
    if !may_take_part {
        return Err(
            "this key may not underwrite (the owner, an insider or an observer, R-PI-4)".into(),
        );
    }
    if whole == 0 {
        return Err("--amount must be at least 1 tONYX".into());
    }
    Ok(())
}

/// The contracts of one markets deployment, as `--markets FILE` names them.
struct Deployment {
    file: std::path::PathBuf,
    doc: Json,
    token: [u8; 20],
    facts: [u8; 20],
    futures: [u8; 20],
}

impl Deployment {
    fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc: Json =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let a = |k: &str| -> Result<[u8; 20], String> {
            parse_addr(
                doc[k]
                    .as_str()
                    .ok_or(format!("{}: no {k}", path.display()))?,
            )
        };
        Ok(Deployment {
            file: path.to_path_buf(),
            token: a("token")?,
            facts: a("facts")?,
            futures: a("futures")?,
            doc,
        })
    }

    fn save(&self) -> Result<(), String> {
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&self.doc).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())
    }

    fn market_mut(&mut self, id: u64) -> Result<&mut Json, String> {
        self.doc["markets"]
            .as_array_mut()
            .and_then(|m| m.iter_mut().find(|e| e["id"].as_u64() == Some(id)))
            .ok_or(format!("market {id} is not in the deployment file"))
    }
}

/// The registry fact a market on `date` settles from (`OnyxRegistry.factKeyOf`
/// under the source's own template) and its subject text. Refused unless the
/// metric is dated, no round may observe `date` in a window starting at the
/// trading close (the value is still unknown to Onyx then, THREATS NL6-1),
/// and one may at `observable`.
pub fn market_fact(
    src: &dyn Source,
    date: &str,
    close: u64,
    observable: u64,
) -> Result<(Hash32, String), String> {
    let t = src.template();
    if MetricKind::of(&t) != MetricKind::DatedFeed {
        return Err("a market names one dated fact: this metric is not dated".into());
    }
    if src.spec(date, close).is_ok() {
        return Err(format!(
            "a round may already observe {date} before trading closes at {close}: no market"
        ));
    }
    let spec = src
        .spec(date, observable)
        .map_err(|e| format!("{date} is not observable at {observable}: {e}"))?;
    let fact = registry_fact_key(
        &metric_key(&t.metric),
        &t.id(),
        &subject_key(&spec.subject),
        0,
        true,
    );
    Ok((fact, subject_text(&spec.subject).to_owned()))
}

/// The round id and number a results store settled for `subject`, checked as
/// the server checks a store (the source's own template pinned, every round
/// re-verified at `now`); refused unless the round's registry fact is `fact`.
pub fn settled_value(
    src: &dyn Source,
    store: &str,
    subject: &str,
    fact: &str,
    now: u64,
) -> Result<(Hash32, i128), String> {
    let t = src.template();
    let pins = TemplatePins::from([(t.metric.0.clone(), t.id())]);
    let feed = ObservationStore::from_json(store, now, &pins).map_err(|e| e.to_string())?;
    let r = feed
        .record(subject, None, None)
        .ok_or(format!("no settled round for {subject}"))?;
    let Answer::Number(value) = r.transcript.answer else {
        return Err("the round settled no number".into());
    };
    if format!("0x{}", hex(&r.checked.registry_fact.0)) != fact.to_lowercase() {
        return Err("the store's round is for another fact".into());
    }
    Ok((r.checked.round_id, value))
}

fn ax(a: &[u8; 20]) -> String {
    format!("0x{}", hex(a))
}

/// ABI calldata of `sig(args)` (`cast calldata`).
fn calldata(chain: &CastChain, sig: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let mut a = vec!["calldata".to_string(), sig.into()];
    a.extend(args.iter().cloned());
    let s = chain.run(&a)?;
    let mut out = vec![0u8; s.trim().trim_start_matches("0x").len() / 2];
    hex_to(s.trim().trim_start_matches("0x"), &mut out).map_err(|e| format!("{sig}: {e}"))?;
    Ok(out)
}

fn send(
    chain: &CastChain,
    who: &KeySigner,
    to: &[u8; 20],
    sig: &str,
    args: &[String],
) -> Result<u64, String> {
    let data = calldata(chain, sig, args)?;
    who.send_call(to, &data).map_err(|e| format!("{sig}: {e}"))
}

/// A read-only call, decoded by `cast call --json` (one JSON string per
/// return value).
fn call(chain: &CastChain, to: &[u8; 20], sig: &str, args: &[String]) -> Result<Vec<Json>, String> {
    let mut a = vec!["call".to_string(), "--json".into(), ax(to), sig.into()];
    a.extend(args.iter().cloned());
    let out = chain.run(&a)?;
    match serde_json::from_str::<Json>(&out).map_err(|e| format!("{sig}: {e}"))? {
        Json::Array(v) => Ok(v),
        v => Ok(vec![v]),
    }
}

fn num(v: &Json, what: &str) -> Result<u128, String> {
    let s = v
        .as_str()
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .ok_or(format!("{what}: not a number: {v}"))?;
    let s = s.split_whitespace().next().unwrap_or_default();
    if let Some(h) = s.strip_prefix("0x") {
        u128::from_str_radix(h, 16).map_err(|e| format!("{what}: {e}"))
    } else {
        s.parse().map_err(|e| format!("{what}: {e}"))
    }
}

fn eth_balance(chain: &CastChain, who: &[u8; 20]) -> Result<u128, String> {
    num(
        &Json::String(chain.run(&["balance".into(), ax(who)])?),
        "balance",
    )
}

fn block_now(chain: &CastChain) -> Result<(u64, u64), String> {
    let n = num(
        &Json::String(chain.run(&["block-number".into()])?),
        "block number",
    )?;
    let n = u64::try_from(n).map_err(|e| e.to_string())?;
    Ok((n, block_time(chain, n)?))
}

fn block_time(chain: &CastChain, n: u64) -> Result<u64, String> {
    let t = chain.run(&[
        "block".into(),
        n.to_string(),
        "-f".into(),
        "timestamp".into(),
    ])?;
    u64::try_from(num(&Json::String(t), "timestamp")?).map_err(|e| e.to_string())
}

fn admin(chain: &CastChain) -> Result<KeySigner, String> {
    let key = std::env::var(ADMIN_KEY_ENV)
        .map_err(|_| format!("{ADMIN_KEY_ENV} is required: the markets admin's key"))?;
    let mut secret = [0u8; 32];
    hex_to(key.trim().trim_start_matches("0x"), &mut secret)
        .map_err(|_| format!("{ADMIN_KEY_ENV}: not a 32-byte hex key"))?;
    KeySigner::new(chain, secret)
}

fn signer_of(chain: &CastChain, path: &str) -> Result<KeySigner, String> {
    KeySigner::new(chain, read_key_file(Path::new(path))?)
}

fn key_files(args: &[String]) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == "--key-file")
        .map(|w| w[1].clone())
        .collect()
}

/// Run one markets command for `src` (see the module docs).
pub fn run(src: &dyn Source, args: &[String]) -> Result<(), String> {
    let cmd = args.get(1).map(String::as_str).unwrap_or_default();
    let rpc = need(args, "--rpc")?;
    let mut chain_args = vec![
        "--rpc".to_string(),
        rpc,
        "--registry".into(),
        "0x0000000000000000000000000000000000000001".into(),
        "--coordinator".into(),
        "0x0000000000000000000000000000000000000001".into(),
    ];
    if let Some(c) = flag(args, "--cast")? {
        chain_args.extend(["--cast".into(), c]);
    }
    let chain = CastChain::from_args(&chain_args)?.ok_or("--rpc URL is required")?;
    let chain_id = chain.chain_id()?;
    if chain_id == ANVIL_CHAIN_ID || chain_id != 84_532 {
        return Err(format!(
            "markets run on Base Sepolia (84532) only, this node is chain {chain_id}"
        ));
    }
    let mut d = Deployment::load(Path::new(&need(args, "--markets")?))?;
    if d.doc["chainId"].as_u64() != Some(chain_id) {
        return Err("the deployment file names another chain".into());
    }
    match cmd {
        "open" => open(src, &chain, &mut d, args),
        "bet" => bet(&chain, &mut d, args),
        "settle" => settle(src, &chain, &mut d, args),
        "status" => status(&chain, &d, args),
        "underwrite" => underwrite(&chain, &d, args),
        _ => Err(
            "usage: market open|bet|settle|status|underwrite --rpc URL --markets FILE ...".into(),
        ),
    }
}

fn open(
    src: &dyn Source,
    chain: &CastChain,
    d: &mut Deployment,
    args: &[String],
) -> Result<(), String> {
    let t = src.template();
    let date = need(args, "--record-date")?;
    let time = |name: &str| -> Result<u64, String> {
        need(args, name)?
            .parse()
            .map_err(|e| format!("{name}: {e}"))
    };
    let (close, observable, resolve_by) = (
        time("--close")?,
        time("--observable")?,
        time("--resolve-by")?,
    );
    let edges: Vec<i128> = need(args, "--edges")?
        .split(',')
        .map(|e| e.trim().parse().map_err(|x| format!("--edges {e}: {x}")))
        .collect::<Result<_, String>>()?;
    let (fact_key, subject) = market_fact(src, &date, close, observable)?;
    let latest_close = close_by(src, &date)?;
    let (head, now) = block_now(chain)?;
    check_close(close, latest_close, now)?;
    // New markets only on guarded futures: insiders may not bet, and the
    // chain refuses a close after `closeBy` (THREATS NL6-10, NL6-12).
    let guard = call(chain, &d.futures, "guard()(address)", &[])
        .map_err(|_| "these futures are not guarded: open new markets on the guarded deployment (docs/ops/deployment.md)".to_string())?;
    if guard.first().and_then(Json::as_str).map(str::to_lowercase) != Some(ax(&d.facts)) {
        return Err("the futures' guard is not this deployment's facts contract".into());
    }
    // The first block a recording may come from: the block at `observable`,
    // from the chain's own recent block rate, so an early recording never
    // proves (THREATS NL6-11; late is safe, a round runs only after it).
    let back = 10_000;
    let then = block_time(chain, head.saturating_sub(back))?;
    let per_block_ms = (now.saturating_sub(then) * 1_000 / back).max(1);
    let not_before = block_at(head, now, observable, per_block_ms);
    let admin = admin(chain)?;
    let owner = call(chain, &d.futures, "owner()(address)", &[])?;
    if owner.first().and_then(Json::as_str).map(str::to_lowercase) != Some(ax(&admin.address())) {
        return Err(format!("{ADMIN_KEY_ENV} is not the futures owner"));
    }
    let (mk, tid) = (metric_key(&t.metric), t.id());
    let sk = subject_key(&src.spec(&date, observable)?.subject);
    let listed = call(
        chain,
        &d.facts,
        "listed(bytes32)(bytes32,bytes32,uint64,uint64)",
        &[format!("0x{}", hex(&fact_key.0))],
    )?;
    if listed.first().and_then(Json::as_str) == Some(&format!("0x{}", hex(&[0u8; 32]))) {
        send(
            chain,
            &admin,
            &d.facts,
            "list(bytes32,bytes32,bytes32,uint64,bool,uint64,uint64)",
            &[
                format!("0x{}", hex(&mk.0)),
                format!("0x{}", hex(&tid.0)),
                format!("0x{}", hex(&sk.0)),
                "0".into(),
                "true".into(),
                not_before.to_string(),
                latest_close.to_string(),
            ],
        )?;
    }
    let id = u64::try_from(num(
        &call(chain, &d.futures, "marketCount()(uint256)", &[])?[0],
        "count",
    )?)
    .map_err(|e| e.to_string())?
        + 1;
    let edge_list = format!(
        "[{}]",
        edges
            .iter()
            .map(i128::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    send(
        chain,
        &admin,
        &d.futures,
        "createMarket(bytes32,int256[],uint64,uint64,uint64)",
        &[
            format!("0x{}", hex(&fact_key.0)),
            edge_list,
            close.to_string(),
            observable.to_string(),
            resolve_by.to_string(),
        ],
    )?;
    let count = num(
        &call(chain, &d.futures, "marketCount()(uint256)", &[])?[0],
        "count",
    )?;
    if count != u128::from(id) {
        return Err(format!("market count {count} after opening market {id}"));
    }
    let entry = json!({
        "id": id,
        "metric": t.metric.0,
        "recordDate": date,
        "subject": subject,
        "fact": format!("0x{}", hex(&fact_key.0)),
        "edges": edges.iter().map(i128::to_string).collect::<Vec<_>>(),
        "tradingClose": close,
        "observableAt": observable,
        "resolveBy": resolve_by,
        "closeBy": latest_close,
        "notBeforeBlock": not_before,
        "bettors": [],
    });
    if !d.doc["markets"].is_array() {
        d.doc["markets"] = json!([]);
    }
    if let Some(m) = d.doc["markets"].as_array_mut() {
        m.push(entry.clone());
    }
    d.save()?;
    println!("{entry}");
    Ok(())
}

fn bet(chain: &CastChain, d: &mut Deployment, args: &[String]) -> Result<(), String> {
    let id: u64 = need(args, "--market")?
        .parse()
        .map_err(|e| format!("--market: {e}"))?;
    let bucket: u8 = need(args, "--bucket")?
        .parse()
        .map_err(|e| format!("--bucket: {e}"))?;
    let whole: u128 = need(args, "--amount")?
        .parse()
        .map_err(|e| format!("--amount: {e}"))?;
    let amount = whole.checked_mul(TONYX).ok_or("--amount too large")?;
    let who = signer_of(chain, &need(args, "--key-file")?)?;
    let me = ax(&who.address());
    // Nothing is granted or minted for a bet the chain would refuse (NL6-14).
    let may = call(
        chain,
        &d.futures,
        "mayBet(address)(bool)",
        std::slice::from_ref(&me),
    )?;
    let s = &call(chain, &d.futures, MARKET_SIG, &[id.to_string()])?[0];
    let close = u64::try_from(num(&s[1], "trading close")?).map_err(|e| e.to_string())?;
    let buckets = s[15].as_array().map_or(0, Vec::len);
    bet_precheck(
        may.first() == Some(&Json::Bool(true)),
        num(&s[4], "state")?,
        close,
        block_now(chain)?.1,
        bucket,
        buckets,
    )?;
    let admin = admin(chain)?;
    let mut sent_gas = false;
    if eth_balance(chain, &who.address())? < GAS_FLOOR_WEI {
        admin.send_value(&who.address(), GAS_GRANT_WEI)?;
        sent_gas = true;
    }
    let held = num(
        &call(
            chain,
            &d.token,
            "balanceOf(address)(uint256)",
            std::slice::from_ref(&me),
        )?[0],
        "balance",
    )?;
    if held < amount {
        send(
            chain,
            &admin,
            &d.token,
            "mint(address,uint256)",
            &[me.clone(), (amount - held).to_string()],
        )?;
    }
    send(
        chain,
        &who,
        &d.token,
        "approve(address,uint256)",
        &[ax(&d.futures), amount.to_string()],
    )?;
    let block = send(
        chain,
        &who,
        &d.futures,
        "bet(uint256,uint8,uint128)",
        &[id.to_string(), bucket.to_string(), amount.to_string()],
    )?;
    let m = d.market_mut(id)?;
    if let Some(b) = m["bettors"].as_array_mut() {
        if !b.iter().any(|x| x.as_str() == Some(&me)) {
            b.push(Json::String(me.clone()));
        }
    }
    d.save()?;
    println!(
        "{}",
        json!({"market": id, "bettor": me, "bucket": bucket, "tonyx": whole.to_string(), "block": block, "gasGrant": sent_gas})
    );
    Ok(())
}

fn underwrite(chain: &CastChain, d: &Deployment, args: &[String]) -> Result<(), String> {
    let ins = parse_addr(
        d.doc["insurance"]
            .as_str()
            .ok_or("no insurance in the deployment file: --markets <export>/insurance.json")?,
    )?;
    let whole: u128 = need(args, "--amount")?
        .parse()
        .map_err(|e| format!("--amount: {e}"))?;
    let amount = whole.checked_mul(TONYX).ok_or("--amount too large")?;
    let who = signer_of(chain, &need(args, "--key-file")?)?;
    let me = ax(&who.address());
    let may = call(
        chain,
        &ins,
        "mayTakePart(address)(bool)",
        std::slice::from_ref(&me),
    )?;
    underwrite_precheck(may.first() == Some(&Json::Bool(true)), whole)?;
    let admin = admin(chain)?;
    let mut sent_gas = false;
    if eth_balance(chain, &who.address())? < GAS_FLOOR_WEI {
        admin.send_value(&who.address(), GAS_GRANT_WEI)?;
        sent_gas = true;
    }
    let held = num(
        &call(
            chain,
            &d.token,
            "balanceOf(address)(uint256)",
            std::slice::from_ref(&me),
        )?[0],
        "balance",
    )?;
    if held < amount {
        send(
            chain,
            &admin,
            &d.token,
            "mint(address,uint256)",
            &[me.clone(), (amount - held).to_string()],
        )?;
    }
    send(
        chain,
        &who,
        &d.token,
        "approve(address,uint256)",
        &[ax(&ins), amount.to_string()],
    )?;
    let block = send(
        chain,
        &who,
        &ins,
        "underwrite(uint256)",
        &[amount.to_string()],
    )?;
    let pool = num(&call(chain, &ins, "pool()(uint256)", &[])?[0], "pool")?;
    let reserved = num(
        &call(chain, &ins, "reserved()(uint256)", &[])?[0],
        "reserved",
    )?;
    println!(
        "{}",
        json!({"insurance": ax(&ins), "underwriter": me, "tonyx": whole.to_string(), "block": block, "gasGrant": sent_gas,
            "pool": pool.to_string(), "reserved": reserved.to_string()})
    );
    Ok(())
}

fn settle(
    src: &dyn Source,
    chain: &CastChain,
    d: &mut Deployment,
    args: &[String],
) -> Result<(), String> {
    let id: u64 = need(args, "--market")?
        .parse()
        .map_err(|e| format!("--market: {e}"))?;
    let m = d.market_mut(id)?.clone();
    let fact = m["fact"].as_str().ok_or("no fact")?.to_string();
    let subject = m["subject"].as_str().ok_or("no subject")?.to_string();
    let path = need(args, "--store")?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let (round, value) = settled_value(src, &text, &subject, &fact, SystemClock.now())
        .map_err(|e| format!("{path}: {e}"))?;
    let admin = admin(chain)?;
    let fv = call(
        chain,
        &d.facts,
        "factValue(bytes32)(bool,int256,uint8,uint64)",
        std::slice::from_ref(&fact),
    )?;
    if fv.first() != Some(&Json::Bool(true)) {
        send(
            chain,
            &admin,
            &d.facts,
            "prove(bytes32,bytes32,int128)",
            &[
                fact.clone(),
                format!("0x{}", hex(&round.0)),
                value.to_string(),
            ],
        )?;
    }
    let state = |c: &CastChain| -> Result<u128, String> {
        num(
            &call(c, &d.futures, MARKET_SIG, &[id.to_string()])?[0][4],
            "state",
        )
    };
    if state(chain)? == 1 {
        send(
            chain,
            &admin,
            &d.futures,
            "resolve(uint256)",
            &[id.to_string()],
        )?;
    }
    let mut paid = vec![];
    for f in key_files(args) {
        let who = signer_of(chain, &f)?;
        let owed = num(
            &call(
                chain,
                &d.futures,
                "owed(uint256,address)(uint256)",
                &[id.to_string(), ax(&who.address())],
            )?[0],
            "owed",
        )?;
        if owed > 0 {
            send(
                chain,
                &who,
                &d.futures,
                "withdraw(uint256)",
                &[id.to_string()],
            )?;
            paid.push(json!({"bettor": ax(&who.address()), "tonyx": owed.to_string()}));
        }
    }
    println!(
        "{}",
        json!({"market": id, "fact": fact, "round": format!("0x{}", hex(&round.0)), "value": value.to_string(), "state": state(chain)?.to_string(), "paid": paid})
    );
    Ok(())
}

fn status(chain: &CastChain, d: &Deployment, args: &[String]) -> Result<(), String> {
    const STATES: [&str; 4] = ["none", "open", "resolved", "void"];
    let mut out = vec![];
    for m in d.doc["markets"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let id = m["id"].as_u64().ok_or("market without id")?;
        let v = call(chain, &d.futures, MARKET_SIG, &[id.to_string()])?;
        let s = &v[0];
        let state = usize::try_from(num(&s[4], "state")?).map_err(|e| e.to_string())?;
        let pools: Vec<String> = s[15]
            .as_array()
            .map(|p| {
                p.iter()
                    .map(|x| num(x, "pool").map(|n| n.to_string()))
                    .collect::<Result<_, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let fv = call(
            chain,
            &d.facts,
            "factValue(bytes32)(bool,int256,uint8,uint64)",
            &[m["fact"].as_str().unwrap_or_default().into()],
        )?;
        out.push(json!({
            "id": id,
            "metric": m["metric"],
            "recordDate": m["recordDate"],
            "subject": m["subject"],
            "fact": m["fact"],
            "edges": m["edges"],
            "tradingClose": m["tradingClose"],
            "observableAt": m["observableAt"],
            "resolveBy": m["resolveBy"],
            "state": STATES.get(state).copied().unwrap_or("unknown"),
            "pools": pools,
            "total": num(&s[9], "total")?.to_string(),
            "bettors": num(&s[7], "bettors")?.to_string(),
            "winner": if state == 2 { json!(num(&s[5], "winner")?.to_string()) } else { Json::Null },
            "value": if fv.first() == Some(&Json::Bool(true)) { json!(fv[1].as_str().unwrap_or_default().split_whitespace().next()) } else { Json::Null },
        }));
    }
    let doc = json!({
        "chainId": d.doc["chainId"],
        "registry": d.doc["registry"],
        "token": d.doc["token"],
        "facts": d.doc["facts"],
        "futures": d.doc["futures"],
        "stakes": d.doc["stakes"],
        "panel": d.doc["panel"],
        "markets": out,
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    match flag(args, "--out")? {
        Some(p) => std::fs::write(&p, text).map_err(|e| format!("{p}: {e}"))?,
        None => print!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The admin's and the coordinator's keys never reach a `cast` child
    /// (THREATS B6-1, NL6).
    #[test]
    fn the_key_variables_are_removed_from_every_cast_child() {
        use onyx_coordinator::cast_chain::KEY_ENV_VARS;
        assert!(KEY_ENV_VARS.contains(&ADMIN_KEY_ENV));
        assert!(KEY_ENV_VARS.contains(&crate::live::COORDINATOR_KEY_ENV));
    }

    /// A market on a day closes before that day starts in UTC, less the
    /// margin: a close after the source may show the value is refused
    /// (THREATS NL6-10).
    #[test]
    fn a_close_after_the_source_publishes_is_refused() {
        let day = crate::start_of_day("2026-10-31").unwrap();
        assert_eq!(day, 1_793_404_800);
        let by = day - CLOSE_MARGIN_SECS;
        let now = day - 86_400;
        assert!(check_close(by, by, now).is_ok());
        assert!(check_close(by + 1, by, now).is_err(), "after closeBy");
        assert!(check_close(now, by, now).is_err(), "not in the future");
        assert!(crate::start_of_day("2026-13-01").is_err());
    }

    /// The first block a recording may come from is the block at
    /// `observableAt`, not at the close (THREATS NL6-11).
    #[test]
    fn the_earliest_recording_block_is_at_the_observable_time() {
        let (head, now) = (1_000_000, 1_793_000_000);
        let (close, observable) = (now + 3_600, now + 86_400);
        // 0.2 s blocks, as on Base Sepolia.
        let at_close = block_at(head, now, close, 200);
        let at_observable = block_at(head, now, observable, 200);
        assert_eq!(at_observable, head + 432_000);
        assert!(at_observable > at_close);
        assert_eq!(
            block_at(head, now, now - 5, 200),
            head,
            "never before the head"
        );
    }

    /// A bet the chain would refuse gets no gas grant and no mint (THREATS
    /// NL6-14): an insider, a closed or settled market, a missing bucket.
    #[test]
    fn a_bet_that_cannot_succeed_gets_no_gas() {
        let (close, now) = (2_000, 1_000);
        assert!(bet_precheck(true, 1, close, now, 2, 3).is_ok());
        assert!(bet_precheck(false, 1, close, now, 0, 3).is_err(), "insider");
        assert!(bet_precheck(true, 2, close, now, 0, 3).is_err(), "resolved");
        assert!(bet_precheck(true, 1, close, close, 0, 3).is_err(), "closed");
        assert!(
            bet_precheck(true, 1, close, now, 3, 3).is_err(),
            "no bucket 3"
        );
    }

    /// An underwriter the pool would refuse gets no gas grant and no mint
    /// (R-PI-4): an insider or observer, or a zero deposit.
    #[test]
    fn an_underwrite_that_cannot_succeed_gets_no_gas() {
        assert!(underwrite_precheck(true, 500).is_ok());
        assert!(underwrite_precheck(false, 500).is_err(), "insider");
        assert!(underwrite_precheck(true, 0).is_err(), "zero");
    }
}
