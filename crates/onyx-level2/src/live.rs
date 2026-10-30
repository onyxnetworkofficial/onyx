//! Live level-2 rounds for any [`Source`] (ROADMAP B2, B6): the coordinator
//! and the independent observer process, moved from the Treasury adapter's
//! examples so every dated source runs them (`examples/level2-coordinator.rs`
//! and `examples/level2-observer.rs` of each adapter call [`coordinator`] and
//! [`observer`]).
//!
//! On a local anvil every sender is an unlocked dev account, as before. On
//! a live chain (Base Sepolia, ROADMAP B6) each sender signs its own
//! transactions with a [`KeySigner`]: the coordinator's key comes from the
//! environment ([`COORDINATOR_KEY_ENV`], never a flag or a file the run
//! writes), the registry owner's from `--owner-key FILE`, and each observer
//! signs with the key it already holds (`--key-file`). Keys stay in the
//! process: never printed, never on a `cast` command line.

use crate::{
    append_attempt, append_results, check_new_store, drawn_round_with, observer_turn, parse_pool,
    run_and_save_with, store, Observers, Page, RoundRun, Source,
};
use onyx_api::observations::{Attempt, ObservationStore};
use onyx_coordinator::cast_chain::{
    anvil_clock, check_clock_skew, independent_clock, parse_addr, receipt_key_from_args, CastChain,
    TxSigner, ANVIL_CHAIN_ID, KEY_ENV_VARS,
};
use onyx_coordinator::keyfile::{key_address, read_key_file, RevealKey};
use onyx_coordinator::observe::{
    lock_book, write_atomic, ChainView, ChainWait, Clock, DrawChain, Posted, SignedReveal,
    SystemClock,
};
use onyx_coordinator::observer_link::{
    parse_reveal, reveal_json, send_reveal, write_private_atomic, DirInbox,
};
use onyx_core::Hash32;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable the live coordinator reads its own key from (hex,
/// 32 bytes). `cast` never sees it ([`CastChain::run`] removes it).
pub const COORDINATOR_KEY_ENV: &str = "ONYX_L2_COORDINATOR_KEY";

/// Highest fee per gas a [`KeySigner`] pays, in wei (1 gwei): a testnet
/// guard; Base Sepolia's fee is far lower.
pub const MAX_FEE_PER_GAS: u128 = 1_000_000_000;

/// A sender that signs its own EIP-1559 transactions (ROADMAP B6) and sends
/// them through `cast rpc` (`eth_sendRawTransaction`): the key never leaves
/// this process. `Debug` shows only the address.
pub struct KeySigner {
    cast: PathBuf,
    rpc: String,
    work: PathBuf,
    chain_id: u64,
    secret: [u8; 32],
    address: [u8; 20],
    nonce: std::sync::Mutex<Option<u64>>,
}

impl std::fmt::Debug for KeySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KeySigner(0x{})", hex(&self.address))
    }
}

impl KeySigner {
    /// A signer for `secret` on `chain`'s node. Refused on a local anvil
    /// (unlocked accounts there) and for a key that is not valid.
    pub fn new(chain: &CastChain, secret: [u8; 32]) -> Result<Self, String> {
        if chain.chain_id()? == ANVIL_CHAIN_ID {
            return Err("a local anvil sends from unlocked accounts; no key is needed".into());
        }
        Self::on_any_chain(chain, secret)
    }

    /// [`Self::new`] on a local anvil too: an outside observer's key is no
    /// unlocked account there (north-star loop NL16 rehearsal).
    pub fn on_any_chain(chain: &CastChain, secret: [u8; 32]) -> Result<Self, String> {
        let chain_id = chain.chain_id()?;
        let address = key_address(&secret).ok_or("not a valid secp256k1 key")?;
        Ok(KeySigner {
            cast: chain.cast.clone(),
            rpc: chain.rpc.clone(),
            work: chain.work.clone(),
            chain_id,
            secret,
            address,
            nonce: std::sync::Mutex::new(None),
        })
    }

    fn rpc(&self, method: &str, params: Json) -> Result<Json, String> {
        let mut cmd = Command::new(&self.cast);
        cmd.current_dir(&self.work)
            .args(["rpc", "--raw", method, &params.to_string()])
            .env("ETH_RPC_URL", &self.rpc);
        for k in KEY_ENV_VARS {
            cmd.env_remove(k);
        }
        let out = cmd.output().map_err(|e| format!("cast: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{method}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("{method}: {e}"))
    }

    fn quantity(&self, v: &Json, what: &str) -> Result<u128, String> {
        let s = v.as_str().ok_or(format!("{what}: not a quantity: {v}"))?;
        u128::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("{what}: {e}"))
    }

    /// Send `wei` of the chain's (test) ETH to `to`, for a new actor's gas
    /// (NS11 markets); waits for the receipt and returns its block number.
    pub fn send_value(&self, to: &[u8; 20], wei: u128) -> Result<u64, String> {
        self.send_tx(to, &[], wei)
    }

    /// Sign and send a call of `data` with `value` wei to `to`; wait for its
    /// receipt and return its block number (a revert is an error).
    fn send_tx(&self, to: &[u8; 20], data: &[u8], value: u128) -> Result<u64, String> {
        let from = format!("0x{}", hex(&self.address));
        let call = json!({
            "from": from,
            "to": format!("0x{}", hex(to)),
            "data": format!("0x{}", hex(data)),
            "value": format!("0x{value:x}"),
        });
        // A revert shows here, before anything is signed.
        let estimate = self.rpc("eth_estimateGas", json!([call]))?;
        let gas = u64::try_from(self.quantity(&estimate, "gas")?)
            .map_err(|e| e.to_string())?
            .saturating_mul(13)
            / 10
            + 10_000;
        let head = self.rpc("eth_getBlockByNumber", json!(["latest", false]))?;
        let base = self.quantity(&head["baseFeePerGas"], "base fee")?;
        let tip = self.quantity(&self.rpc("eth_maxPriorityFeePerGas", json!([]))?, "tip")?;
        let max_fee = base.saturating_mul(2).saturating_add(tip);
        if max_fee > MAX_FEE_PER_GAS {
            return Err(format!(
                "fee {max_fee} wei per gas is above the {MAX_FEE_PER_GAS} wei cap"
            ));
        }
        let mut held = self.nonce.lock().map_err(|_| "nonce lock poisoned")?;
        let nonce = match *held {
            Some(n) => n,
            None => u64::try_from(self.quantity(
                &self.rpc("eth_getTransactionCount", json!([from, "pending"]))?,
                "nonce",
            )?)
            .map_err(|e| e.to_string())?,
        };
        let tx = onyx_api::evm_tx::Tx1559 {
            chain_id: self.chain_id,
            nonce,
            max_priority_fee_per_gas: tip.min(max_fee),
            max_fee_per_gas: max_fee,
            gas,
            to: *to,
            value,
            data: data.to_vec(),
        };
        let (raw, hash) = tx.sign(&self.secret).ok_or("signing failed")?;
        let hash = format!("0x{}", hex(&hash));
        if let Err(e) = self.rpc(
            "eth_sendRawTransaction",
            json!([format!("0x{}", hex(&raw))]),
        ) {
            // The nonce is read again next time (the send may or may not
            // have landed).
            *held = None;
            return Err(e);
        }
        *held = Some(nonce + 1);
        drop(held);
        for _ in 0..240 {
            let r = self.rpc("eth_getTransactionReceipt", json!([hash]))?;
            if !r.is_null() {
                let block = u64::try_from(self.quantity(&r["blockNumber"], "block")?)
                    .map_err(|e| e.to_string())?;
                if r["status"].as_str() != Some("0x1") {
                    return Err(format!("reverted in tx {hash}"));
                }
                // Public RPCs balance reads over several nodes: give the
                // others a moment to hold the block before the caller reads.
                std::thread::sleep(std::time::Duration::from_secs(2));
                return Ok(block);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(format!("no receipt for {hash} after 120 s"))
    }
}

impl TxSigner for KeySigner {
    fn address(&self) -> [u8; 20] {
        self.address
    }

    fn send_call(&self, to: &[u8; 20], data: &[u8]) -> Result<u64, String> {
        self.send_tx(to, data, 0)
    }
}

/// Add a [`KeySigner`] for `secret` to `chain` unless it is a local anvil.
/// Returns the signer's address.
fn add_signer(chain: &mut CastChain, secret: [u8; 32]) -> Result<Option<[u8; 20]>, String> {
    if chain.chain_id()? == ANVIL_CHAIN_ID {
        return Ok(None);
    }
    add_key_signer(chain, secret)
}

/// The key signs in this process on any chain, a local anvil too: an outside
/// observer's key is no unlocked anvil account (north-star loop NL16).
fn add_key_signer(chain: &mut CastChain, secret: [u8; 32]) -> Result<Option<[u8; 20]>, String> {
    let s = KeySigner::on_any_chain(chain, secret)?;
    let a = s.address;
    chain.signers.push(std::sync::Arc::new(s));
    Ok(Some(a))
}

pub(crate) fn flag(args: &[String], name: &str) -> Result<Option<String>, String> {
    args.iter()
        .position(|a| a == name)
        .map(|i| {
            args.get(i + 1)
                .cloned()
                .ok_or(format!("{name} needs a value"))
        })
        .transpose()
}

pub(crate) fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name)?.ok_or(format!("{name} is required"))
}

/// The version of a round announcement (north-star loop NL16).
pub const ANNOUNCEMENT_V: u64 = 1;

/// The round announcement an outside observer joins with (`--announce`,
/// docs/join.md): the chain, the registry, the fact's metric, template and
/// series, the record date, the window, the pool snapshot and how reveals
/// travel. Nothing in it is a secret, and the observer trusts none of it
/// without a check ([`apply_announcement`]).
#[allow(clippy::too_many_arguments)]
pub fn announcement(
    src: &dyn Source,
    chain_id: u64,
    registry: &str,
    rpc: &str,
    record_date: &str,
    window_start: u64,
    pool_text: &str,
    sealed: bool,
) -> Result<Json, String> {
    let t = src.template();
    let pool: Json = serde_json::from_str(pool_text).map_err(|e| format!("pool: {e}"))?;
    parse_pool(pool_text)?;
    Ok(json!({
        "v": ANNOUNCEMENT_V,
        "chainId": chain_id,
        "registry": registry,
        "rpc": rpc,
        "metric": t.metric.0,
        "templateId": t.id().to_string(),
        "series": std::env::var("ONYX_L2_SERIES").unwrap_or_default(),
        "recordDate": record_date,
        "windowStart": window_start,
        "pool": pool,
        "reveals": if sealed { "sealed" } else { "spool" },
    }))
}

/// An observer's `--announcement FILE` (north-star loop NL16): the round's
/// flags from the coordinator's announcement, after the checks a stranger
/// can make without trusting it. Refused: another version, a round of
/// another metric or template than this program observes (its own copy),
/// spool reveals without the observer's own `--spool` (an outside observer
/// cannot write the coordinator's folder), and any round flag given twice.
/// Returns the expanded flags and the chain id the RPC must report.
pub fn apply_announcement(
    src: &dyn Source,
    args: &[String],
) -> Result<(Vec<String>, Option<u64>), String> {
    let Some(file) = flag(args, "--announcement")? else {
        return Ok((args.to_vec(), None));
    };
    for f in ["--pool", "--record-date", "--window-start", "--registry"] {
        if flag(args, f)?.is_some() {
            return Err(format!("{f} comes from --announcement: give one, not both"));
        }
    }
    let text = std::fs::read_to_string(&file).map_err(|e| format!("--announcement {file}: {e}"))?;
    if text.len() > 1 << 20 {
        return Err(format!("--announcement {file}: larger than 1 MiB"));
    }
    let a: Json = serde_json::from_str(&text).map_err(|e| format!("--announcement {file}: {e}"))?;
    let s = |k: &str| {
        a[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("--announcement: no {k}"))
    };
    let u = |k: &str| {
        a[k].as_u64()
            .ok_or_else(|| format!("--announcement: no {k}"))
    };
    if u("v")? != ANNOUNCEMENT_V {
        return Err(format!(
            "--announcement: version {}, this observer reads {ANNOUNCEMENT_V}",
            a["v"]
        ));
    }
    let t = src.template();
    if s("metric")? != t.metric.0 || s("templateId")? != t.id().to_string() {
        return Err(format!(
            "--announcement: the round is for {} (template {}), this observer observes {} (template {}): get the program for that source and series",
            s("metric")?, s("templateId")?, t.metric.0, t.id()
        ));
    }
    let mut out = args.to_vec();
    match s("reveals")?.as_str() {
        "sealed" => {
            if flag(args, "--spool")?.is_none() {
                // Never written: sealed reveals go on chain. The observer
                // still names a folder of its own.
                let dir = PathBuf::from(need(args, "--state")?).join("spool");
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                out.extend(["--spool".into(), dir.display().to_string()]);
            }
        }
        "spool" if flag(args, "--spool")?.is_some() => {}
        r => {
            return Err(format!(
                "--announcement: reveals \"{r}\" need a transfer to the coordinator's spool; an outside observer joins sealed rounds only (docs/join.md)"
            ))
        }
    }
    let pool_text = serde_json::to_string(&a["pool"]).map_err(|e| e.to_string())?;
    parse_pool(&pool_text)?;
    let state = PathBuf::from(need(args, "--state")?);
    std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let pool_path = state.join("announced-pool.json");
    write_atomic(&pool_path, &pool_text)?;
    // Review CL4-R1-5: the seats of a keyed round, checked against the pool before any commit.
    if !a["seats"].is_null() {
        if flag(args, "--seats")?.is_some() {
            return Err("--seats comes from --announcement: give one, not both".into());
        }
        let seats_path = state.join("announced-seats.json");
        write_atomic(
            &seats_path,
            &serde_json::to_string(&a["seats"]).map_err(|e| e.to_string())?,
        )?;
        out.extend(["--seats".into(), seats_path.display().to_string()]);
    }
    out.extend([
        "--pool".into(),
        pool_path.display().to_string(),
        "--record-date".into(),
        s("recordDate")?,
        "--window-start".into(),
        u("windowStart")?.to_string(),
        "--registry".into(),
        s("registry")?,
    ]);
    if flag(args, "--rpc")?.is_none() {
        out.extend(["--rpc".into(), s("rpc")?]);
    }
    Ok((out, Some(u("chainId")?)))
}

/// `--record-date latest|YYYY-MM-DD` for a window starting at `window_start`.
fn record_date(src: &dyn Source, args: &[String], window_start: u64) -> Result<String, String> {
    Ok(match need(args, "--record-date")?.as_str() {
        "latest" => src
            .latest(window_start)
            .ok_or("no record date the rule allows for this window")?,
        d => d.to_string(),
    })
}

/// The live level-2 coordinator for independent observers (ROADMAP B2, B6).
/// Flags as the Treasury adapter's `examples/level2-coordinator.rs` documents,
/// plus, on a live chain: the coordinator's key in [`COORDINATOR_KEY_ENV`]
/// (its address must be `--coordinator`) and `--owner-key FILE` (the
/// registry owner's, when `--owner` registers the metric; its address must be
/// `--owner`). Prints the round's outcome as JSON; `--pin` prints only the
/// node's template pin for the metric.
pub fn coordinator(src: &dyn Source, args: &[String]) -> Result<(), String> {
    let t = src.template();
    // `--pin`: the node's pin for this metric's store (`metric=0x<template
    // id>`, ONYX_OBSERVATION_TEMPLATES), printed without running a round.
    if args.iter().any(|a| a == "--pin") {
        println!("{}={}", t.metric.0, t.id());
        return Ok(());
    }
    let book = need(args, "--book")?;
    let spool = need(args, "--spool")?;
    let window_start: u64 = need(args, "--window-start")?
        .parse()
        .map_err(|e| format!("--window-start: {e}"))?;
    let record_date = record_date(src, args, window_start)?;
    // ADR-0054 (CL4): a keyed source draws from the round's keyed snapshot of `--seats`; with
    // too few drawable seats there is no round, and the value stays level 4, said so.
    let (pool_text, pool) = if src.key_placement().is_some() {
        let sp = src.spec(&record_date, window_start)?;
        match keyed_pool(src, args, &sp)? {
            Ok(p) => p,
            Err(statement) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&statement).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
        }
    } else {
        let pool_text =
            std::fs::read_to_string(need(args, "--pool")?).map_err(|e| format!("--pool: {e}"))?;
        let pool = parse_pool(&pool_text)?;
        (pool_text, pool)
    };
    if flag(args, "--receipt-key")?.is_none() {
        return Err(
            "--receipt-key FILE is required: the deployment's own receipt key (ROADMAP B3)".into(),
        );
    }
    // One runner per book, taken before anything is sent (review R10-4).
    let _lock = lock_book(Path::new(&book))?;
    let mut cast = CastChain::from_args(args)?
        .ok_or("--rpc URL is required: the coordinator announces its draw on OnyxRegistry")?;
    // ROADMAP B6: on a live chain the coordinator and the owner sign their own.
    if cast.chain_id()? != ANVIL_CHAIN_ID {
        let key = std::env::var(COORDINATOR_KEY_ENV).map_err(|_| {
            format!("{COORDINATOR_KEY_ENV} is required on a live chain: the coordinator's key")
        })?;
        let mut secret = [0u8; 32];
        hex_to(key.trim().trim_start_matches("0x"), &mut secret)
            .map_err(|_| format!("{COORDINATOR_KEY_ENV}: not a 32-byte hex key"))?;
        if add_signer(&mut cast, secret)? != Some(cast.coordinator) {
            return Err(format!(
                "{COORDINATOR_KEY_ENV} is not the key of --coordinator"
            ));
        }
        if let Some(owner) = flag(args, "--owner")? {
            let file = flag(args, "--owner-key")?
                .ok_or("--owner on a live chain needs --owner-key FILE")?;
            let secret = read_key_file(Path::new(&file))?;
            if add_signer(&mut cast, secret)? != Some(parse_addr(&owner)?) {
                return Err("--owner-key is not the key of --owner".into());
            }
        }
    }
    // ROADMAP B3: checked before anything is sent.
    let members: Vec<[u8; 20]> = pool.iter().map(|o| o.address).collect();
    let receipt_key = receipt_key_from_args(args, Some(&mut cast), &members)?;
    if members.contains(&cast.coordinator) {
        return Err(
            "the coordinator is a pool member: refused (it would observe its own round)".into(),
        );
    }
    // ROADMAP C11: the reveal key, a key of its own, checked before anything
    // is sent; the draws this run announces name it.
    let reveal_key = match flag(args, "--reveal-key")? {
        None => None,
        Some(p) => {
            let k = RevealKey::for_chain(Some(cast.chain_id()?), Path::new(&p))?;
            let mut named: Vec<(&str, [u8; 20])> = vec![
                ("the receipt key", receipt_key.address()),
                ("the coordinator", cast.coordinator),
                (
                    "the registry owner",
                    parse_addr(&cast.call("owner()(address)", &[])?)?,
                ),
            ];
            for (who, f) in [
                ("the registry owner", "--owner"),
                ("the reporter", "--reporter"),
            ] {
                if let Some(a) = flag(args, f)? {
                    named.push((who, parse_addr(&a)?));
                }
            }
            named.extend(members.iter().map(|a| ("an observer", *a)));
            k.check_distinct(&named)?;
            cast.reveal_key = Some(k.point());
            Some(k)
        }
    };
    cast.record_rounds = true;
    // The keys the draws name are on the registry before the draw is made.
    cast.sync_keys()?;
    // ROADMAP C51: one way; every later draw is keyed and time-locked.
    if args.iter().any(|a| a == "--require-time-lock") {
        if reveal_key.is_none() {
            return Err("--require-time-lock needs --reveal-key: every draw would need one".into());
        }
        cast.require_time_lock()?;
        eprintln!(
            "the registry requires time-locks (one way): every draw from now on is time-locked"
        );
    }
    // ROADMAP B2: independent observers reveal in real time.
    let shifted;
    let clock: &dyn Clock = match flag(args, "--clock")?.as_deref() {
        Some("anvil") => {
            shifted = anvil_clock(&cast)?;
            &shifted
        }
        other => {
            independent_clock(other)?;
            &SystemClock
        }
    };
    // Review B2-R2-2: reveals are stamped by this clock but sent by chain
    // time; a clock far from the chain's would drop every honest reveal.
    check_clock_skew(clock.now(), cast.latest_block()?.time)?;
    if let Some(owner) = flag(args, "--owner")? {
        cast.register_metric(&parse_addr(&owner)?, &t)?;
    }
    let mut inbox = DirInbox::open(Path::new(&spool))?;
    let mut st = store(src);
    st.load_book(Path::new(&book))?;
    let store_path = flag(args, "--store")?;
    let new_store = args.iter().any(|a| a == "--new-store");
    let onchain = cast.deployment()?;
    let written = match &store_path {
        Some(p) => ObservationStore::read_file(Path::new(p), new_store)?,
        None => None,
    };
    if store_path.is_some() && written.is_none() {
        check_new_store(
            src,
            &st,
            &record_date,
            window_start,
            &cast,
            &ChainWait::DEFAULT,
        )?;
    }
    let mut publish = |r: &RoundRun| -> Result<(), String> {
        if let Some(path) = &store_path {
            let prev = ObservationStore::read_file(Path::new(path), new_store)?;
            let next = ObservationStore::tagged(append_results(src, prev.as_ref(), r)?, onchain)
                .map_err(|e| format!("{path}: {e}"))?;
            let s = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
            write_atomic(Path::new(path), &format!("{s}\n"))?;
            eprintln!("wrote the results store {path}");
        }
        Ok(())
    };
    let mut publish_failed = |a: &Attempt| -> Result<(), String> {
        if let Some(path) = &store_path {
            let prev = ObservationStore::read_file(Path::new(path), new_store)?;
            let next = ObservationStore::tagged(append_attempt(src, prev.as_ref(), a)?, onchain)
                .map_err(|e| format!("{path}: {e}"))?;
            let s = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
            write_atomic(Path::new(path), &format!("{s}\n"))?;
            eprintln!("wrote the failed round to the results store {path}");
        }
        Ok(())
    };
    // North-star loop NL16 (docs/join.md): the round as an outside observer
    // needs it, written before the draw is announced. It holds no secret.
    if let Some(path) = flag(args, "--announce")? {
        let mut a = announcement(
            src,
            cast.chain_id()?,
            &need(args, "--registry")?,
            &need(args, "--rpc")?,
            &record_date,
            window_start,
            &pool_text,
            reveal_key.is_some(),
        )?;
        // Review CL4-R1-5: a keyed round's announcement carries the seats, so every keyed
        // observer recomputes the snapshot and refuses any other pool.
        if src.key_placement().is_some() {
            let seats = std::fs::read_to_string(need(args, "--seats")?)
                .map_err(|e| format!("--seats: {e}"))?;
            a["seats"] = serde_json::from_str(&seats).map_err(|e| format!("--seats: {e}"))?;
        }
        let s = serde_json::to_string_pretty(&a).map_err(|e| e.to_string())?;
        write_atomic(Path::new(&path), &format!("{s}\n"))?;
        eprintln!("wrote the round announcement {path}");
    }
    eprintln!(
        "coordinator 0x{}: drawing {} of {} pool members for {} {record_date} (window {window_start}..{}), receipt signer 0x{}, {}",
        hex(&cast.coordinator),
        t.min_observers,
        pool.len(),
        t.metric.0,
        window_start + t.window_secs,
        hex(&receipt_key.address()),
        reveal_key.as_ref().map_or("no reveal key (spool reveals)".to_string(), |k| format!("sealed reveals to key 0x{}", hex(&k.address())))
    );
    let chain: &mut dyn DrawChain = &mut cast;
    let r = run_and_save_with(
        src,
        &record_date,
        window_start,
        Observers::Independent(&pool),
        &mut st,
        chain,
        Path::new(&book),
        written.as_ref(),
        &ChainWait::DEFAULT,
        clock,
        &mut inbox,
        &receipt_key,
        reveal_key.as_ref(),
        &mut publish,
        &mut publish_failed,
    )
    .map_err(|e| {
        if e.unfinished.is_some() {
            format!("{}; the round is kept in the book {book}: run again with the same --book to finish it on the same draw", e.error)
        } else {
            e.error
        }
    })?;
    let mut out = json!({
        "metric": t.metric.0,
        // The pin a node serving the store needs (ONYX_OBSERVATION_TEMPLATES).
        "template": t.id().to_string(),
        "record_date": record_date,
        "round": r.round_id.to_string(),
        "attempt": r.draw.attempt,
        "outcome": format!("{:?}", r.settlement.outcome()),
        "observers": r.settlement.statuses().iter().map(|(a, s)| json!({
            "address": format!("0x{}", hex(a)),
            "status": format!("{s:?}"),
        })).collect::<Vec<_>>(),
        "commits_from_chain": r.commits.len(),
        "reveals": r.reveals.len(),
        "level2_claim": r.claim.is_some(),
        // ROADMAP C11: the round's sealed posts as read past reveal close.
        "sealed_posts": r.sealed.posts.len(),
        "dismissed": r.sealed.dismissals.iter().map(|(a, _)| format!("0x{}", hex(a))).collect::<Vec<_>>(),
        "recorded_attempt": r.sealed.record.map(|d| d.to_string()),
        // ROADMAP C51: the lock round, and how many dismissals carry openings.
        "time_locked": r.sealed.time_lock.is_some(),
        "lock_round": r.sealed.time_lock.as_ref().map(|t| t.lock_round),
        // ROADMAP C52: the far round of every post's far copy (the fact's).
        "far_round": r.sealed.time_lock.as_ref().map(|t| t.far_round),
        "openings": r.sealed.time_lock.as_ref().map_or(0, |t| t.openings.len()),
    });
    if src.key_placement().is_some() {
        out["keyed"] = crate::keyed::keyed_shown(pool.len(), t.min_observers);
        out["proof_level"] = json!(if r.claim.is_some() { 2 } else { 4 });
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// The pool of a keyed round (ADR-0054, CL4): the round's keyed snapshot of `--seats`
/// ([`crate::keyed::snapshot_for`]: signed seats for this source and terms, bond, bond age,
/// history, one seat per operator). A `--pool` given too must equal it. `Ok(Err(statement))`
/// when too few seats are drawable: no round, the level-4 statement instead.
#[allow(clippy::type_complexity)]
fn keyed_pool(
    src: &dyn Source,
    args: &[String],
    sp: &onyx_core::ObservationSpec,
) -> Result<Result<(String, Vec<onyx_coordinator::observe::Observer>), Json>, String> {
    use crate::keyed::{one_holder_statement, pool_to_json, snapshot_for, KeyedError};
    let path = need(args, "--seats")
        .map_err(|_| "a keyed source draws from --seats FILE (the holders' signed seats)")?;
    let seats = std::fs::read_to_string(&path).map_err(|e| format!("--seats: {e}"))?;
    match snapshot_for(src, &seats, sp) {
        Err(e @ KeyedError::TooFewSeats { have, .. }) => {
            Ok(Err(one_holder_statement(src, have, &e.to_string())))
        }
        Err(e) => Err(e.to_string()),
        Ok(s) => {
            for (a, why) in &s.left_out {
                eprintln!("keyed seat 0x{}: left out of this round ({why})", hex(a));
            }
            if let Some(p) = flag(args, "--pool")? {
                let given =
                    parse_pool(&std::fs::read_to_string(&p).map_err(|e| format!("--pool: {e}"))?)?;
                if given != s.pool {
                    return Err(
                        "--pool is not this round's keyed snapshot of --seats (ADR-0054 item 6)"
                            .into(),
                    );
                }
            }
            Ok(Ok((pool_to_json(&s.pool)?, s.pool)))
        }
    }
}

/// One independent level-2 observer (ROADMAP B2, B6): the process's exit
/// code. Flags as the Treasury adapter's `examples/level2-observer.rs`
/// documents, plus `--receipt-wait SECS` (how long past the reveal window
/// it waits for the coordinator's receipt; default 600, and a live chain's
/// coordinator receipts only once the commit phase is final, about 20
/// minutes on Base Sepolia) and `--address` (print the key's address and
/// exit). On a live chain it signs its own commit with its `--key-file` key.
pub fn observer(src: &dyn Source, args: &[String]) -> i32 {
    observer_with(src, args, None)
}

/// [`observer`] with another transport for the source fetch (`None`: the live
/// [`onyx_fetch::HttpsFetch::new`]); the local keyed demo passes its loopback fixture. A
/// keyed source (ADR-0054, CL4) needs `--api-key-stdin`: the holder's own provider key on
/// standard input (`onyx-contrib keyed observe` hands it from the vault), never a flag, a
/// file or the environment. It is held as an [`onyx_fetch::ApiKey`] and goes only into the
/// fetch to the template's host.
pub fn observer_with(
    src: &dyn Source,
    args: &[String],
    transport: Option<onyx_fetch::HttpsFetch>,
) -> i32 {
    match observer_run(src, args, transport) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("observer: {e}");
            1
        }
    }
}

fn observer_run(
    src: &dyn Source,
    args: &[String],
    transport: Option<onyx_fetch::HttpsFetch>,
) -> Result<i32, String> {
    let key = read_key_file(Path::new(&need(args, "--key-file")?))?;
    let me = key_address(&key).ok_or("not a valid key")?;
    if args.iter().any(|a| a == "--address") {
        println!("0x{}", hex(&me));
        return Ok(0);
    }
    // ADR-0054 item 2 (THREATS CL4-1): the provider key, from standard input only.
    let api_key = if args.iter().any(|a| a == "--api-key-stdin") {
        Some(onyx_fetch::ApiKey::from_reader(std::io::stdin().lock()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let keyed = match (src.key_placement(), api_key) {
        (Some(p), Some(k)) => Some((p, k)),
        (Some(_), None) => {
            return Err("a keyed source: give your own provider key on standard input with --api-key-stdin (onyx-contrib keyed observe does it from your vault)".into())
        }
        (None, Some(_)) => return Err("--api-key-stdin: this source is public and takes no key".into()),
        (None, None) => None,
    };
    // North-star loop NL16: an outside observer joins with the announcement.
    let (expanded, announced_chain) = apply_announcement(src, args)?;
    let args = expanded.as_slice();
    let tag = format!("observer 0x{}", &hex(&me)[..8]);
    let pool = parse_pool(
        &std::fs::read_to_string(need(args, "--pool")?).map_err(|e| format!("--pool: {e}"))?,
    )?;
    if !pool.iter().any(|o| o.address == me) {
        return Err(format!("{tag}: not a member of the pool snapshot"));
    }
    let spool = PathBuf::from(need(args, "--spool")?);
    let state = PathBuf::from(need(args, "--state")?);
    let window_start: u64 = need(args, "--window-start")?
        .parse()
        .map_err(|e| format!("--window-start: {e}"))?;
    let receipt_wait: u64 = flag(args, "--receipt-wait")?
        .map_or(Ok(600), |s| s.parse())
        .map_err(|e| format!("--receipt-wait: {e}"))?;
    let record_date = record_date(src, args, window_start)?;
    let sp = src.spec(&record_date, window_start)?;
    let cfg = src.observe_config(&sp);
    // ADR-0054 item 6, review CL4-R1-5: a keyed observer always checks the pool is the
    // round's keyed snapshot of the seats (the operator rule, bond age and history), never
    // just any pool a coordinator announces.
    if keyed.is_some() {
        let seats = match flag(args, "--seats")? {
            Some(p) => Some(std::fs::read_to_string(&p).map_err(|e| format!("--seats: {e}"))?),
            None => None,
        };
        crate::keyed::check_observer_pool(src, seats.as_deref(), &sp, &pool)
            .map_err(|e| format!("{tag}: {e}"))?;
    }
    // The observer's own chain reader; its sender is its own account.
    let mut chain_args = args.to_vec();
    chain_args.extend(["--coordinator".into(), format!("0x{}", hex(&me))]);
    let mut chain = CastChain::from_args(&chain_args)?.ok_or("--rpc URL is required")?;
    if let Some(id) = announced_chain {
        let got = chain.chain_id()?;
        if got != id {
            return Err(format!(
                "your RPC is on chain {got}, the round announcement names chain {id}: refused"
            ));
        }
        add_key_signer(&mut chain, key)?;
    } else {
        add_signer(&mut chain, key)?;
    }
    let fact = store(src).onchain_fact(&sp).map_err(|e| e.to_string())?;
    let shifted;
    let clock: &dyn Clock = match flag(args, "--clock")?.as_deref() {
        None | Some("system") => &SystemClock,
        Some("anvil") => {
            shifted = anvil_clock(&chain)?;
            &shifted
        }
        Some(c) => {
            return Err(format!(
                "--clock {c}: use system (or anvil, local rehearsals)"
            ))
        }
    };
    // Review B2-R2-2: this observer fetches and commits by its own clock
    // and reveals by chain time; refuse a clock far from the chain's.
    check_clock_skew(clock.now(), chain.latest_block()?.time)?;

    // 2. The draw, read and checked by the observer itself.
    let mut seen_other: Vec<u32> = vec![];
    let (seed, id) = loop {
        // Polls retry (a public RPC answers an error now and then).
        let n = ChainWait::DEFAULT.retry(|| chain.draw_count(&fact))?;
        let latest = match n {
            0 => None,
            n => ChainWait::DEFAULT.retry(|| chain.draw_of(&fact, n - 1))?,
        };
        if let Some(rec) = latest.filter(|r| r.beacon != Hash32::ZERO) {
            match drawn_round_with(&sp, &cfg, &pool, &rec, &me) {
                Ok(Some((seed, _, id))) => break (seed, id),
                Ok(None) => {
                    eprintln!("{tag}: not drawn for attempt {}", rec.attempt);
                    // Review CL4-R1-3: an undrawn keyed seat still fetches once at its
                    // own private slot and discards the page, so the keys that fetch in
                    // a window are the whole snapshot, never just the drawn addresses.
                    if let Some(k) = &keyed {
                        let slot = crate::keyed::random_fetch_time(&sp)?;
                        if slot >= clock.now() && slot <= sp.window_end {
                            clock.wait_until(slot);
                            let _ = fetch(
                                &sp.url,
                                src.fetch_limits(),
                                transport.as_ref(),
                                Some(k),
                                &tag,
                            );
                            eprintln!("{tag}: cover fetch done (not drawn; the page is discarded)");
                        }
                    }
                    return Ok(0);
                }
                // An earlier draw of the fact (another window: a re-draw's
                // observers start before it is announced) or a draw this
                // observer must not commit to: never committed to; the
                // observer keeps reading until commit close (ROADMAP C11
                // rehearsal of a re-draw).
                Err(e) if !seen_other.contains(&rec.attempt) => {
                    eprintln!(
                        "{tag}: draw {} is not this round's ({e}); waiting",
                        rec.attempt
                    );
                    seen_other.push(rec.attempt);
                }
                Err(_) => {}
            }
        }
        if clock.now() > cfg.commit_close {
            return Err(format!("{tag}: no fixed draw before commit close"));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    eprintln!("{tag}: drawn in round {id} (attempt {})", seed.attempt);

    // 3. Fetch, observe, sign, and save before sending anything.
    std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let saved = state.join(format!("{id}.json"));
    let (commit, commit_sig, reveal) = match load(&saved, &id, &me)? {
        Some(t) => {
            eprintln!("{tag}: resuming the saved commit and reveal");
            t
        }
        None => {
            // Partner tracker M2: each observer fetches at its own slot in
            // the window, not all at the window start (onyx_core::fetch).
            // ADR-0054 item 10 (THREATS CL4-7): a keyed observer's slot is
            // its own random draw, never derived from public data.
            let slot = match &keyed {
                Some(_) => crate::keyed::random_fetch_time(&sp)?,
                None => onyx_core::fetch::fetch_time(&sp, &id, &me),
            };
            clock.wait_until(slot);
            let fetched_unix = clock.now();
            if fetched_unix > sp.window_end {
                return Err(format!("{tag}: the window closed before the fetch"));
            }
            let body = match flag(args, "--body")? {
                Some(p) => Some(std::fs::read(&p).map_err(|e| format!("--body: {e}"))?),
                None => fetch(
                    &sp.url,
                    src.fetch_limits(),
                    transport.as_ref(),
                    keyed.as_ref(),
                    &tag,
                ),
            };
            // Review CL4-R1-1: a value that holds a piece of the key is never committed.
            let body = match &keyed {
                Some((_, k)) => {
                    let had = body.is_some();
                    let b = crate::keyed::keyed_page(src, &sp, body, k);
                    if had && b.is_none() {
                        eprintln!(
                            "{tag}: the value read holds a piece of the key: answering Unavailable"
                        );
                    }
                    b
                }
                None => body,
            };
            let t = observer_turn(
                src,
                &sp,
                &id,
                seed.attempt,
                &key,
                &Page { fetched_unix, body },
            )
            .map_err(|e| match &keyed {
                Some((_, k)) => k.redact(&e),
                None => e,
            })?;
            save(&saved, &id, seed.attempt, &t)?;
            t
        }
    };

    // Review CL4-R1-2: a keyed observer posts its commit at the window's end, whatever its
    // private fetch second, so the commit's time says nothing about when its key fetched.
    if keyed.is_some() {
        clock.wait_until(crate::keyed::keyed_commit_time(&sp));
    }
    // 4. The commit, posted on chain by the observer itself. Review
    // C11-R1-5: a restart past commit close goes on when its own commit is
    // already on chain (step 5 checks it is inside the phase), so a crash
    // after the commit never loses the reveal.
    if clock.now() > cfg.commit_close {
        let height = chain.height()?;
        let posted = ChainWait::DEFAULT
            .retry(|| chain.posted_commit(height, &fact, seed.attempt, &me, &id))?;
        if posted.map(|p| p.commit) != Some(commit) {
            return Err(format!(
                "{tag}: past commit close; the commit would not count"
            ));
        }
    }
    match ChainWait::DEFAULT
        .retry(|| chain.post_commit(&fact, seed.attempt, &me, &id, &commit, &commit_sig))?
    {
        Posted::Now => eprintln!("{tag}: posted commit {commit}"),
        Posted::Before(c) if c == commit => eprintln!("{tag}: commit already posted"),
        Posted::Before(c) => {
            return Err(format!(
                "{tag}: the slot holds another commit {c} (this key was used elsewhere)"
            ))
        }
    }

    // 5. The reveal, only once the CHAIN is past commit close (review
    // B2-R1-M1: no commit can still land in the phase, so nobody who reads
    // the reveal can post a matching commit in time, whatever this host's
    // clock says) and this observer's own commit is on chain inside the
    // phase (otherwise the reveal could only be a non-reveal).
    let head = loop {
        let b = ChainWait::DEFAULT.retry(|| chain.latest_block())?;
        if b.time > cfg.commit_close {
            break b;
        }
        if clock.now() > cfg.reveal_close {
            return Err(format!(
                "{tag}: the chain never passed commit close in the reveal window; nothing sent"
            ));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    match ChainWait::DEFAULT
        .retry(|| chain.posted_commit(head.number, &fact, seed.attempt, &me, &id))?
    {
        Some(p)
            if p.commit == commit
                && p.posted_at >= sp.window_start
                && p.posted_at <= cfg.commit_close => {}
        _ => {
            eprintln!(
                "{tag}: my commit is not on chain inside the commit phase at block {}: not revealing (it could only be a non-reveal)",
                head.number
            );
            return Ok(4);
        }
    }
    if clock.now() > cfg.reveal_close {
        return Err(format!("{tag}: past reveal close; nothing sent"));
    }

    // ROADMAP C11: a draw with a reveal key takes the reveal ON CHAIN, sealed.
    if let Some(reveal_key) =
        ChainWait::DEFAULT.retry(|| chain.draw_reveal_key(&fact, seed.attempt))?
    {
        return sealed_reveal(
            src, args, &mut chain, clock, &tag, &key, &me, &state, &sp, &cfg, &fact, &seed, &id,
            &reveal, reveal_key,
        );
    }
    let path = send_reveal(&spool, &id, seed.attempt, &reveal)?;
    eprintln!("{tag}: reveal sent ({})", path.display());

    // 6. The coordinator's receipt, on chain. Until reveal close the reveal
    // is written again on every poll (review B2-R1-M2: a file removed or
    // garbled by someone else with access to the spool is restored; the
    // coordinator keeps the first reveal it took).
    let deadline = cfg.reveal_close + receipt_wait;
    loop {
        if let Some(c) =
            ChainWait::DEFAULT.retry(|| chain.receipt_posted(&fact, seed.attempt, &me, &id))?
        {
            if c == commit {
                eprintln!("{tag}: receipt on chain for commit {c}");
                return Ok(0);
            }
            return Err(format!("{tag}: a receipt for another commit {c}"));
        }
        if clock.now() <= cfg.reveal_close {
            if let Err(e) = send_reveal(&spool, &id, seed.attempt, &reveal) {
                eprintln!("{tag}: writing the reveal again: {e}");
            }
        }
        if clock.now() > deadline {
            eprintln!(
                "{tag}: NO receipt {receipt_wait} s after the reveal window: the coordinator did not take or did not finish this reveal (THREATS REVLOG-1); the posted commit is the evidence"
            );
            return Ok(3);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// Step 5 for a draw with a reveal key (ROADMAP C11, C51, C52): seal the
/// reveal, save it, post it on chain and check it landed inside the phase.
#[allow(clippy::too_many_arguments)]
fn sealed_reveal(
    src: &dyn Source,
    args: &[String],
    chain: &mut CastChain,
    clock: &dyn Clock,
    tag: &str,
    key: &[u8; 32],
    me: &[u8; 20],
    state: &Path,
    sp: &onyx_core::ObservationSpec,
    cfg: &onyx_coordinator::observe::ObserveConfig,
    fact: &onyx_coordinator::observe::OnchainFact,
    seed: &onyx_coordinator::observe::DrawSeed,
    id: &Hash32,
    reveal: &SignedReveal,
    reveal_key: onyx_coordinator::sealed::Point,
) -> Result<i32, String> {
    let garbage = args.iter().any(|a| a == "--sealed-garbage");
    let raw = args.iter().any(|a| a == "--sealed-garbage-raw");
    if (garbage || raw) && chain.chain_id()? != ANVIL_CHAIN_ID {
        return Err("--sealed-garbage is for a local anvil rehearsal only".into());
    }
    // ROADMAP C51: a time-locked draw's post is locked to drand quicknet's
    // lock round of THIS window's reveal close (the config the on-chain
    // draw commits to, checked by `drawn_round`). ROADMAP C52: and its
    // body key to the far round of the record date's last observable
    // day, from the feed's own template (never a coordinator input).
    let time_locked = ChainWait::DEFAULT.retry(|| chain.draw_time_locked(fact, seed.attempt))?;
    let tl = if time_locked {
        let q = onyx_coordinator::timelock::QuicknetTimeLock::pinned()
            .map_err(|e| format!("{tag}: {e}"))?;
        let rounds = onyx_coordinator::timelock::LockRounds::of_round(
            &onyx_coordinator::timelock::BeaconChain::quicknet(),
            &src.template(),
            sp,
            cfg.reveal_close,
        )
        .map_err(|e| format!("{tag}: {e}"))?;
        eprintln!(
            "{tag}: time-locked to quicknet round {} (reveal close) and far round {} (unseal)",
            rounds.lock(),
            rounds.far()
        );
        Some((q, rounds))
    } else {
        None
    };
    let sealed_path = state.join(format!("{id}.sealed.json"));
    let post = match load_sealed(&sealed_path, me)? {
        Some(p) => p,
        None => {
            let (_, o, salt, _) = reveal;
            let junk = vec![0x5a; onyx_coordinator::sealed::sealed_len(sp)];
            let fresh = || -> Result<[u8; 32], String> {
                Ok(onyx_coordinator::observe::random_salt()
                    .map_err(|e| format!("ephemeral key: {e}"))?
                    .0)
            };
            let p = match (&tl, garbage, raw) {
                // Junk with no valid time-lock in front (or an amendment
                // 10 junk post): the same length as an honest post.
                (_, _, true) | (None, true, _) => {
                    let mut j = junk.clone();
                    if tl.is_some() {
                        j.extend([0x5a; onyx_coordinator::sealed::TIME_LOCKED_HEAD_BYTES]);
                    }
                    onyx_coordinator::sealed::seal_bytes(
                        key,
                        me,
                        &fresh()?,
                        &reveal_key,
                        id,
                        seed.attempt,
                        &j,
                    )
                    .map_err(|e| e.to_string())
                }
                (None, false, false) => onyx_coordinator::sealed::seal_random(
                    key,
                    &reveal_key,
                    id,
                    seed.attempt,
                    sp,
                    o,
                    salt,
                )
                .map_err(|e| e.to_string()),
                // Junk behind a valid time-lock.
                (Some((q, rounds)), true, false) => {
                    let extra = fresh()?;
                    let more = fresh()?;
                    let mut tau = [0u8; 16];
                    let mut sigma = [0u8; 16];
                    let mut kappa = [0u8; 16];
                    tau.copy_from_slice(&extra[..16]);
                    sigma.copy_from_slice(&extra[16..]);
                    kappa.copy_from_slice(&more[..16]);
                    onyx_coordinator::timelock::seal_bytes_timelocked(
                        q,
                        key,
                        me,
                        &onyx_coordinator::timelock::LockSecrets {
                            ephemeral: fresh()?,
                            tau,
                            sigma,
                            kappa,
                        },
                        &reveal_key,
                        id,
                        seed.attempt,
                        *rounds,
                        clock.now(),
                        &junk,
                    )
                    .map_err(|e| e.to_string())
                }
                (Some((q, rounds)), false, false) => {
                    onyx_coordinator::timelock::seal_timelocked_random(
                        q,
                        key,
                        &reveal_key,
                        id,
                        seed.attempt,
                        *rounds,
                        clock.now(),
                        sp,
                        o,
                        salt,
                    )
                    .map_err(|e| e.to_string())
                }
            }
            .map_err(|e| format!("{tag}: sealing: {e}"))?;
            // Saved (0600) before it is sent: a restart posts the same.
            write_private_atomic(&sealed_path, &sealed_json(&p))?;
            p
        }
    };
    let want = onyx_core::keccak256(&post.ciphertext);
    match ChainWait::DEFAULT.retry(|| chain.post_sealed_reveal(fact, seed.attempt, id, &post))? {
        Posted::Now => eprintln!(
            "{tag}: posted sealed reveal ({} bytes{})",
            post.ciphertext.len(),
            if time_locked { ", time-locked" } else { "" }
        ),
        Posted::Before(h) if h == want => eprintln!("{tag}: sealed reveal already posted"),
        Posted::Before(h) => {
            return Err(format!(
                "{tag}: the slot holds another sealed post (ciphertext {h}; this key was used elsewhere)"
            ))
        }
    }
    let on = chain
        .sealed_post_latest(fact, seed.attempt, me, id)?
        .ok_or_else(|| format!("{tag}: my sealed post is not on chain"))?;
    if on.ciphertext != post.ciphertext || on.ephemeral != post.ephemeral {
        return Err(format!("{tag}: the chain holds another sealed post"));
    }
    if on.posted_at <= cfg.commit_close || on.posted_at > cfg.reveal_close {
        eprintln!(
            "{tag}: my sealed post landed at {} outside the reveal phase ({}, {}]: it does not count",
            on.posted_at, cfg.commit_close, cfg.reveal_close
        );
        return Ok(3);
    }
    eprintln!(
        "{tag}: sealed post on chain at {} (the post is the receipt; a round that leaves it out fails revealLogComplete)",
        on.posted_at
    );
    Ok(0)
}

/// A client's check of one served level-2 value (ROADMAP B6): fetch
/// `GET {--url}/v1/observations/{metric}?subject={--subject}&outcome=all`
/// from a node, check it off chain with THIS source's own template (never
/// one the server sends: `verify_observation`), then on chain with the
/// client's own calls to the registry (`observationLevel`, `drawMatches`,
/// `drawBudget`, `transcriptComplete`, the sealed-reveal reads, and the
/// fact's earlier draws). A copy with another value must be refused.
/// Flags: `--url BASE --subject S --rpc URL --registry ADDR [--cast PATH]`.
/// Prints the checked value as JSON.
pub fn client_check(src: &dyn Source, args: &[String]) -> Result<(), String> {
    use onyx_api::client::{
        draw_count_from_return, verify_attempts_onchain, verify_observation,
        verify_observation_onchain, ObservationExpect, SealedCalls, SealedRets,
        VerifiedObservation,
    };
    let t = src.template();
    let base = need(args, "--url")?;
    let subject = need(args, "--subject")?;
    // A client sends nothing: the sender is never used.
    let mut chain_args = args.to_vec();
    chain_args.extend([
        "--coordinator".into(),
        "0x0000000000000000000000000000000000000001".into(),
    ]);
    let chain = CastChain::from_args(&chain_args)?.ok_or("--rpc URL is required")?;
    let url = format!(
        "{}/v1/observations/{}?subject={subject}&outcome=all",
        base.trim_end_matches('/'),
        t.metric.0
    );
    let out = Command::new("curl")
        .args(["-sS", "--fail", "-m", "30", "-A", "onyx-dev", &url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let body: Json = serde_json::from_slice(&out.stdout).map_err(|e| format!("{url}: {e}"))?;
    let subj = onyx_core::Subject::Aggregate(subject.clone());
    let expect = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: SystemClock.now(),
    };
    let sealed_rets = |calls: SealedCalls| -> Result<SealedRets, String> {
        let ask = |d: &[u8]| chain.eth_call(d, None);
        let mut posts = vec![];
        for (r, p) in &calls.posts {
            posts.push((ask(r)?, ask(p)?));
        }
        Ok(SealedRets {
            time_locked: ask(&calls.time_locked)?,
            reveal_key: ask(&calls.reveal_key)?,
            reveal_log: ask(&calls.reveal_log)?,
            matches: match &calls.matches {
                Some(m) => Some(ask(m)?),
                None => None,
            },
            posts,
        })
    };
    let onchain = |v: &VerifiedObservation| -> Result<(), String> {
        let ask = |d: &[u8]| chain.eth_call(d, None);
        let rets: Vec<Vec<u8>> = v
            .onchain_calls()
            .iter()
            .map(|d| ask(d))
            .collect::<Result<_, _>>()?;
        let sealed = sealed_rets(v.sealed_calls())?;
        verify_observation_onchain(v, &rets[0], &rets[1], &rets[2], &rets[3], &sealed)
            .map_err(|e| format!("value round: {e}"))?;
        let a = v
            .attempts
            .as_ref()
            .ok_or("no attempts (asked with outcome=all)")?;
        let count_ret = ask(&a.draw_count_calldata())?;
        let count = draw_count_from_return(&count_ret).map_err(|e| e.to_string())?;
        let draws: Vec<Vec<u8>> = (0..a.draws_without_value(count))
            .map(|i| ask(&a.draw_of_calldata(i)))
            .collect::<Result<_, _>>()?;
        let complete: Vec<Vec<u8>> = a
            .transcript_complete_calldatas()
            .iter()
            .map(|d| ask(d))
            .collect::<Result<_, _>>()?;
        let sealed: Vec<SealedRets> = a
            .sealed_calls()
            .into_iter()
            .map(&sealed_rets)
            .collect::<Result<_, _>>()?;
        verify_attempts_onchain(
            a,
            &count_ret,
            &draws,
            &ask(&a.draw_budget_calldata())?,
            &complete,
            &sealed,
        )
        .map(|_| ())
        .map_err(|e| format!("earlier rounds: {e}"))
    };
    let v = verify_observation(&body, &expect).map_err(|e| format!("off chain: {e}"))?;
    onchain(&v)?;
    // A server that changes the value is caught.
    let mut forged = body.clone();
    forged["value"] = json!(format!("{}1", v.value));
    if verify_observation(&forged, &expect)
        .map_err(|e| e.to_string())
        .and_then(|f| onchain(&f))
        .is_ok()
    {
        return Err("a forged value passed the client's checks".into());
    }
    // ADR-0048: a server that lifts the value's level is caught too.
    let ladder = onyx_api::ladder::to_json(&v.ladder, onyx_api::ladder::OBSERVATION_RECORD);
    let mut lifted = body.clone();
    lifted["ladder"] = ladder.clone();
    lifted["ladder"]["level"] = json!(1);
    lifted["ladder"]["steps"][0]["level"] = json!(1);
    lifted["ladder"]["steps"][0]["name"] = json!("recomputed");
    if verify_observation(&lifted, &expect).is_ok() {
        return Err("a forged level passed the client's checks".into());
    }
    // THREATS NS5-9: a client that keeps the ladders it accepted catches a
    // node that later drops a step (a fall) of the same value.
    if let Some(dir) = flag(args, "--ladder-state")? {
        keep_ladder(std::path::Path::new(&dir), &v.ladder)?;
    }
    println!(
        "{}",
        json!({
            "metric": t.metric.0,
            "subject": subject,
            "value": v.value,
            "round": v.checked.round_id.to_string(),
            "attempt": v.checked.draw.attempt,
            "earlier_rounds": v.attempts.as_ref().map_or(0, |a| a.attempts.len()),
            "forged_value_refused": true,
            "forged_level_refused": true,
            "ladder": ladder,
            "client_checks": "passed (off chain with this source's template; on chain at the registry)",
        })
    );
    Ok(())
}

type Turn = (Hash32, [u8; 65], SignedReveal);

/// A sealed post as the observer saves it (ROADMAP C11).
fn sealed_json(p: &onyx_coordinator::sealed::SealedPost) -> String {
    json!({
        "observer": format!("0x{}", hex(&p.observer)),
        "ephemeral": format!("0x{}", hex(&p.ephemeral)),
        "ciphertext": format!("0x{}", hex(&p.ciphertext)),
        "key_proof": format!("0x{}", hex(&p.key_proof)),
        "sig": format!("0x{}", hex(&p.sig)),
    })
    .to_string()
}

/// The saved sealed post, if any (this observer's own only).
fn load_sealed(
    path: &Path,
    me: &[u8; 20],
) -> Result<Option<onyx_coordinator::sealed::SealedPost>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let v: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let h = |k: &str| -> Result<Vec<u8>, String> {
        decode(
            v[k].as_str()
                .ok_or(format!("sealed state: no {k}"))?
                .trim_start_matches("0x"),
        )
    };
    let observer: [u8; 20] = h("observer")?
        .try_into()
        .map_err(|_| "sealed state: observer")?;
    if observer != *me {
        return Err("sealed state: another observer's post".into());
    }
    Ok(Some(onyx_coordinator::sealed::SealedPost {
        observer,
        ephemeral: h("ephemeral")?
            .try_into()
            .map_err(|_| "sealed state: ephemeral")?,
        ciphertext: h("ciphertext")?,
        key_proof: h("key_proof")?
            .try_into()
            .map_err(|_| "sealed state: key proof")?,
        sig: h("sig")?.try_into().map_err(|_| "sealed state: sig")?,
    }))
}

/// Save the observer's commit and reveal for `round` (0600, atomic).
fn save(path: &Path, round: &Hash32, attempt: u32, t: &Turn) -> Result<(), String> {
    let (commit, sig, reveal) = t;
    let v = json!({
        "commit": commit.to_string(),
        "commit_sig": format!("0x{}", hex(sig)),
        "reveal": serde_json::from_str::<Json>(&reveal_json(round, attempt, reveal)?)
            .map_err(|e| e.to_string())?,
    });
    let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    // The reveal holds the answer the paid API sells: 0600 from the start.
    write_private_atomic(path, &text)
}

/// The saved commit and reveal for `round`, if any.
fn load(path: &Path, round: &Hash32, me: &[u8; 20]) -> Result<Option<Turn>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let v: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let h = |k: &str| -> Result<Vec<u8>, String> {
        let s = v[k].as_str().ok_or(format!("state: no {k}"))?;
        decode(s.trim_start_matches("0x"))
    };
    let commit: [u8; 32] = h("commit")?.try_into().map_err(|_| "state: commit")?;
    let sig: [u8; 65] = h("commit_sig")?.try_into().map_err(|_| "state: sig")?;
    let reveal_v = &v["reveal"];
    // Only this observer's own reveal for this round (review B2-R1-L5).
    let reveal = parse_reveal(&reveal_v.to_string(), round, me)
        .ok_or("state: the saved reveal does not parse")?;
    Ok(Some((Hash32(commit), sig, reveal)))
}

/// One HTTPS fetch of the pinned record through the SSRF-safe transport (CL3, ADR-0053 item 7,
/// `onyx_fetch`): one resolution, public addresses only, connect by address, https on 443, no
/// redirects, at most the source's cap on the wire and after gzip, 20 s for the whole fetch, the
/// neutral User-Agent `onyx-observer/1`. `None` (no answer) on any refusal, logged.
/// A keyed source (ADR-0054) adds the holder's own key in the place its template names, for
/// this one request to the template's host ([`onyx_fetch::HttpsFetch::get_keyed`]); the error
/// text has the key removed. `transport` replaces the live one (the local keyed demo only).
fn fetch(
    url: &str,
    limits: onyx_fetch::Limits,
    transport: Option<&onyx_fetch::HttpsFetch>,
    keyed: Option<&(onyx_fetch::KeyPlacement, onyx_fetch::ApiKey)>,
    tag: &str,
) -> Option<Vec<u8>> {
    let f = match transport {
        Some(t) => Ok(t.clone()),
        None => onyx_fetch::HttpsFetch::new(limits),
    };
    let got = f.and_then(|f| match keyed {
        Some((place, key)) => f.get_keyed_blocking(url, place, key),
        None => f.get_blocking(url),
    });
    match got {
        Ok(body) => Some(body),
        Err(e) => {
            eprintln!("{tag}: no answer from the source: {e}");
            None
        }
    }
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn hex_to(s: &str, out: &mut [u8]) -> Result<(), String> {
    let v = decode(s)?;
    if v.len() != out.len() {
        return Err("length".into());
    }
    out.copy_from_slice(&v);
    Ok(())
}

fn decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// THREATS NS5-9 (ROADMAP C64): `ladder` must extend the ladder of the same
/// value this client accepted before and kept in `dir` (one file per value
/// key); it is then kept in its place.
pub fn keep_ladder(dir: &std::path::Path, ladder: &onyx_core::Ladder) -> Result<(), String> {
    use onyx_api::ladder::{parse, to_json, OBSERVATION_RECORD};
    let path = dir.join(format!("{}.json", ladder.value));
    let at = |e: &dyn std::fmt::Display| format!("--ladder-state {}: {e}", path.display());
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let prev: Json = serde_json::from_str(&s).map_err(|e| at(&e))?;
            let prev = parse(&prev, OBSERVATION_RECORD).map_err(|e| at(&e))?;
            ladder
                .check_extends(&prev)
                .map_err(|e| format!("against the ladder accepted before: {e}"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(at(&e)),
    }
    std::fs::create_dir_all(dir).map_err(|e| at(&e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, to_json(ladder, OBSERVATION_RECORD).to_string()).map_err(|e| at(&e))?;
    std::fs::rename(&tmp, &path).map_err(|e| at(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::{Ladder, ProofLevel, Step};

    #[test]
    fn a_second_run_with_a_dropped_step_is_refused() {
        let dir = std::env::temp_dir().join(format!("onyx-l2-ladders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let v = onyx_core::ladder::value_key(&Hash32([1; 32]), &Hash32([2; 32]));
        let two = Step {
            level: ProofLevel::ReObserved,
            evidence: Hash32([3; 32]),
            at: 100,
        };
        let mut seen = Ladder::new(
            v,
            Step {
                level: ProofLevel::SourceProven,
                evidence: Hash32([4; 32]),
                at: 50,
            },
        )
        .expect("ok");
        seen.raise(two).expect("ok");
        keep_ladder(&dir, &seen).expect("first run");
        keep_ladder(&dir, &seen).expect("same ladder again");
        // A node that drops the level-3 start, or the level-2 rise: a fall.
        let only_two = Ladder::new(v, two).expect("ok");
        let err = keep_ladder(&dir, &only_two).expect_err("refused");
        assert!(err.contains("accepted before"), "{err}");
        let only_three = Ladder::new(v, seen.steps[0]).expect("ok");
        assert!(keep_ladder(&dir, &only_three).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
