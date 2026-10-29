//! The NWS forecast of the Denver high as a testnet forecaster (north-star
//! loop NL4, ADR-0036 amendment 8): each run turns the forecast the caller
//! fetched into signed statements on the station's coming days, puts them
//! through the track-record intake on Base Sepolia (batch headers posted as
//! calldata by a dedicated poster, `committed_at` from the final blocks that
//! carry them), and writes the publication a node serves on `/v1/track`.
//!
//! Testnet only, labelled: the statements are relayed by an Onyx test key
//! (the NWS signs nothing), and NOAA runs both the forecast and the NCEI
//! record that grades it (THREATS TR-R2-4, TRS-6, TRS-7), so a score is a
//! demonstration of the mechanism, never an independent track record.
//! Resolutions come only from final level-2 rounds (`onyx_track::daily`);
//! NCEI days are observable 30 to 60 days late, so the first can come in
//! December 2026 and this run adds none.
//!
//! `cargo run -p onyx-source-ncei --example track-nws -- --forecast FILE
//! --state DIR --out FILE --rpc URL --registry ADDR --coordinator ADDR
//! [--cast PATH] [--no-post]`, run by `scripts/track-nws-sepolia.sh`.
//! Keys come from files named in the environment and are never printed:
//! `ONYX_TRACK_RELAY_KEY_FILE` (signs statements), `ONYX_TRACK_POSTER_KEY_FILE`
//! (posts headers; posts nothing else, its nonce counts the posts), and on a
//! first run `ONYX_TRACK_FUNDER_KEY` (hex, the deployer) to give the poster gas.

use onyx_coordinator::cast_chain::{CastChain, Finality, TxSigner};
use onyx_coordinator::keyfile::read_key_file;
use onyx_coordinator::track_chain::{check_post_tx, write_publication, CastRecord};
use onyx_core::{
    keccak256, Confidence, DataRights, Hash32, KeyProvenance, MetricId, MetricInfo, Redistribution,
    SourceId, SourceKey, StaticRegistry, Value, ValueKind,
};
use onyx_labels::attest::{eip191_sign_with, secret_address};
use onyx_level2::live::KeySigner;
use onyx_source_ncei::nws::{self, Forecast, FORECAST_SOURCE};
use onyx_source_ncei::{rights as ncei_rights, subject, METRIC, SCALE, SUBJECT};
use onyx_track::daily::{daily_fact, daily_rule, DAILY_LEAD_SECS, DAY};
use onyx_track::intake::{
    key_id, Intake, IntakeId, KeyBook, SignedStatement, StatementPayload, TrackKey,
};
use onyx_track::publication::Publication;
use onyx_track::{compute, Fact, TrackParams};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};

/// Base Sepolia.
const CHAIN_ID: u64 = 84_532;
/// The sink the headers go to: an address no one holds a key for
/// (`keccak256("onyx-track-nws-sink-v1")[..20]`), so it never sends.
fn sink() -> [u8; 20] {
    let h = keccak256(b"onyx-track-nws-sink-v1");
    let mut a = [0u8; 20];
    a.copy_from_slice(&h.0[..20]);
    a
}
/// The confidence every relayed statement declares (the NWS states a value,
/// not a probability; ADR-0036 amendment 8 item 3).
const CONFIDENCE_PPM: u32 = 500_000;
/// Tolerance of a daily high: 5 tenths, 0.5 °C (amendment 7 item 1).
const TOLERANCE: u128 = 5;
/// No statement is sent closer than this to its close (a post must be final
/// before the close; Base Sepolia finalizes in about 20 minutes).
const CLOSE_MARGIN_SECS: u64 = 2 * 3_600;
/// The poster's gas, sent once by the funder (test ETH, in wei).
const POSTER_GAS_WEI: u128 = 20_000_000_000_000;
/// Origin the relay key is listed under (this deployment's site).
const RELAY_ORIGIN: &str = "onyx.cryptagraph.com";

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn key_from_env(var: &str) -> Result<[u8; 32], String> {
    let path = std::env::var_os(var).ok_or(format!("{var} is not set"))?;
    read_key_file(Path::new(&path)).map_err(|e| format!("{var}: {e}"))
}

/// The NWS forecaster's rights: public domain, the NWS named, relayed by Onyx.
fn nws_rights() -> DataRights {
    DataRights {
        license: "onyx:us-government-work-noaa".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(FORECAST_SOURCE.into()),
        attribution: Some(
            "Forecast: U.S. National Weather Service, grid BOU 75,66, relayed by an Onyx testnet key; not an official NOAA product as shown here"
                .into(),
        ),
        terms: String::new(),
    }
}

/// The registry of these records: the metric, the two sources, and every day
/// a statement in `days` names.
fn registry(days: &[String]) -> Result<StaticRegistry, String> {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: SCALE },
            needs_label_set: false,
        },
    )
    .map_err(|e| e.to_string())?;
    for d in days {
        r.add_aggregate(&subject(d)).map_err(|e| e.to_string())?;
    }
    r.add_source(nws_rights()).map_err(|e| e.to_string())?;
    r.add_source(ncei_rights()).map_err(|e| e.to_string())?;
    Ok(r)
}

/// The relay key as the level-3 review lists it: under this deployment's own
/// origin, the document being the relay statement in the repo.
fn reviewed(relay: [u8; 20]) -> Vec<SourceKey> {
    vec![SourceKey {
        source: SourceId(FORECAST_SOURCE.into()),
        signer: relay,
        provenance: KeyProvenance::WellKnown {
            origin: RELAY_ORIGIN.into(),
            document_hash: keccak256(include_bytes!("../../../docs/ops/nws-relay.md")),
            // The review record's date, 2026-10-28 00:00 UTC.
            fetched_unix: 1_793_145_600,
        },
    }]
}

fn params(intake: Hash32) -> TrackParams {
    TrackParams::new(vec![daily_rule(MetricId(METRIC.into()), TOLERANCE)])
        .with_level2_resolvers_only()
        .with_intake(intake)
}

/// One run's posts: its statements (submitted at `submitted_at`) and, per
/// sealed batch, the header hash and the transaction that carried it.
#[derive(Clone, Debug)]
struct Run {
    submitted_at: u64,
    statements: Vec<SignedStatement>,
    headers: Vec<Hash32>,
    txs: Vec<Hash32>,
}

struct State {
    poster: [u8; 20],
    first_nonce: u64,
    runs: Vec<Run>,
}

fn load_state(path: &Path) -> Result<Option<State>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let v: Json = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let h32 = |s: &Json| -> Result<Hash32, String> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(s.as_str().ok_or("hash")?.trim_start_matches("0x"), &mut b)
            .map_err(|e| e.to_string())?;
        Ok(Hash32(b))
    };
    let mut poster = [0u8; 20];
    hex::decode_to_slice(
        v["poster"]
            .as_str()
            .ok_or("poster")?
            .trim_start_matches("0x"),
        &mut poster,
    )
    .map_err(|e| e.to_string())?;
    let mut runs = Vec::new();
    for r in v["runs"].as_array().ok_or("runs")? {
        let statements = r["statements"]
            .as_array()
            .ok_or("statements")?
            .iter()
            .map(|s| {
                let b = hex::decode(s.as_str().ok_or("statement")?.trim_start_matches("0x"))
                    .map_err(|e| e.to_string())?;
                borsh::from_slice::<SignedStatement>(&b).map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        let list = |k: &str| -> Result<Vec<Hash32>, String> {
            r[k].as_array()
                .ok_or(k.to_string())?
                .iter()
                .map(h32)
                .collect()
        };
        runs.push(Run {
            submitted_at: r["submittedAt"].as_u64().ok_or("submittedAt")?,
            statements,
            headers: list("headers")?,
            txs: list("txs")?,
        });
    }
    Ok(Some(State {
        poster,
        first_nonce: v["firstNonce"].as_u64().ok_or("firstNonce")?,
        runs,
    }))
}

fn save_state(path: &Path, s: &State) -> Result<(), String> {
    let runs: Vec<Json> = s
        .runs
        .iter()
        .map(|r| {
            json!({
                "submittedAt": r.submitted_at,
                "statements": r.statements.iter().map(|x| hex0x(&borsh::to_vec(x).expect("borsh"))).collect::<Vec<_>>(),
                "headers": r.headers.iter().map(|h| hex0x(&h.0)).collect::<Vec<_>>(),
                "txs": r.txs.iter().map(|h| hex0x(&h.0)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let v = json!({
        "chainId": CHAIN_ID,
        "sink": hex0x(&sink()),
        "poster": hex0x(&s.poster),
        "firstNonce": s.first_nonce,
        "runs": runs,
    });
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{v:#}\n")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn sign(
    secret: &[u8; 32],
    intake: Hash32,
    relay: [u8; 20],
    fact: Fact,
    tenths: i128,
) -> Result<SignedStatement, String> {
    let source = SourceId(FORECAST_SOURCE.into());
    let payload = StatementPayload {
        intake,
        key: key_id(&source, &relay),
        source,
        fact,
        value: Value::Fixed {
            mantissa: tenths,
            scale: SCALE,
        },
        confidence: Confidence(CONFIDENCE_PPM),
    };
    let signature =
        eip191_sign_with(secret, payload.message().as_bytes()).ok_or("signing failed")?;
    Ok(SignedStatement { payload, signature })
}

fn day_of(s: &SignedStatement) -> String {
    match &s.payload.fact.subject {
        onyx_core::Subject::Aggregate(a) => a
            .strip_prefix(SUBJECT)
            .and_then(|x| x.strip_prefix(':'))
            .unwrap_or(a)
            .to_string(),
        other => format!("{other:?}"),
    }
}

/// The transaction of post `seq` in `block`: the poster's with nonce
/// `first_nonce + seq` (checked again by `check_post_tx`).
fn find_post(rec: &CastRecord, block: u64, seq: u64, header: &Hash32) -> Result<Hash32, String> {
    let b = rec.rpc(
        "eth_getBlockByNumber",
        &[format!("\"0x{block:x}\""), "true".into()],
    )?;
    let want = rec.id.first_nonce + seq;
    for t in b["transactions"]
        .as_array()
        .ok_or("block without transactions")?
    {
        let from = t["from"].as_str().unwrap_or_default().to_lowercase();
        let nonce = t["nonce"]
            .as_str()
            .and_then(|n| u64::from_str_radix(n.trim_start_matches("0x"), 16).ok());
        if from == hex0x(&rec.id.poster) && nonce == Some(want) {
            check_post_tx(t, &rec.id, seq, Some(header))?;
            let mut h = [0u8; 32];
            hex::decode_to_slice(
                t["hash"].as_str().ok_or("hash")?.trim_start_matches("0x"),
                &mut h,
            )
            .map_err(|e| e.to_string())?;
            return Ok(Hash32(h));
        }
    }
    Err(format!("post {seq} not found in block {block}"))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let forecast_path = PathBuf::from(arg(&args, "--forecast").ok_or("--forecast FILE")?);
    let state_dir = PathBuf::from(arg(&args, "--state").ok_or("--state DIR")?);
    let out = PathBuf::from(arg(&args, "--out").ok_or("--out FILE")?);
    let post = !args.iter().any(|a| a == "--no-post");
    let chain =
        CastChain::from_args(&args)?.ok_or("--rpc URL --registry ADDR --coordinator ADDR")?;
    if chain.chain_id()? != CHAIN_ID {
        return Err("Base Sepolia (chain 84532) only".into());
    }
    if chain.finality != Finality::Finalized {
        return Err("the intake reads only final blocks: --finality finalized".into());
    }
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let state_path = state_dir.join("state.json");

    let relay_secret = key_from_env("ONYX_TRACK_RELAY_KEY_FILE")?;
    let relay = secret_address(&relay_secret).ok_or("relay key is not valid")?;
    let poster_secret = key_from_env("ONYX_TRACK_POSTER_KEY_FILE")?;
    let poster = KeySigner::new(&chain, poster_secret)?;

    let mut state = match load_state(&state_path)? {
        Some(s) => {
            if s.poster != poster.address() {
                return Err("the poster key is not the one this state started with".into());
            }
            s
        }
        None => {
            let mut rec0 = CastRecord {
                cast: chain.cast.clone(),
                rpc: chain.rpc.clone(),
                work: chain.work.clone(),
                id: IntakeId {
                    chain_id: CHAIN_ID,
                    sink: sink(),
                    poster: poster.address(),
                    first_nonce: 0,
                },
                finality: Finality::Finalized,
                txs: Vec::new(),
            };
            let latest = rec0.block("latest")?;
            rec0.id.first_nonce = rec0.nonce_at(latest.block)?;
            if rec0.id.first_nonce != 0 {
                return Err("the poster has sent before: use a fresh key".into());
            }
            State {
                poster: poster.address(),
                first_nonce: rec0.id.first_nonce,
                runs: Vec::new(),
            }
        }
    };
    let mut rec = CastRecord {
        cast: chain.cast.clone(),
        rpc: chain.rpc.clone(),
        work: chain.work.clone(),
        id: IntakeId {
            chain_id: CHAIN_ID,
            sink: sink(),
            poster: state.poster,
            first_nonce: state.first_nonce,
        },
        finality: Finality::Finalized,
        txs: Vec::new(),
    };
    let intake_id = rec.id.id();
    let p = params(intake_id);

    let forecast: Forecast = nws::parse_max_temperature(
        &std::fs::read(&forecast_path).map_err(|e| format!("{}: {e}", forecast_path.display()))?,
    )?;
    let stated: std::collections::BTreeSet<String> = state
        .runs
        .iter()
        .flat_map(|r| r.statements.iter().map(day_of))
        .collect();
    let mut days: Vec<String> = stated.iter().cloned().collect();
    days.extend(forecast.highs.iter().map(|h| h.day.clone()));
    days.sort();
    days.dedup();
    let r = registry(&days)?;
    let rv = reviewed(relay);
    let book = KeyBook {
        keys: vec![TrackKey {
            source: SourceId(FORECAST_SOURCE.into()),
            signer: relay,
            revoked: false,
        }],
    };
    let mut intake = Intake::new(intake_id, book, &r, &rv).map_err(|e| e.to_string())?;

    // Replay the earlier runs: the same statements at the same times seal into
    // the same headers, carried by the recorded transactions.
    for run in &state.runs {
        for s in &run.statements {
            intake
                .submit(&p, &r, s.clone(), run.submitted_at)
                .map_err(|e| format!("replay: {e}"))?;
        }
        for (h, tx) in run.headers.iter().zip(&run.txs) {
            let got = intake.seal().ok_or("replay: fewer batches than recorded")?;
            if got.hash() != *h {
                return Err("replay: a batch header differs from the one posted".into());
            }
            rec.txs.push(*tx);
        }
        if run.headers.len() != run.txs.len() {
            return Err("replay: a header without its transaction".into());
        }
    }
    if intake.seal().is_some() {
        return Err("replay: a statement was never posted".into());
    }

    if post {
        let now = rec.block("latest")?.timestamp;
        let mut fresh = Vec::new();
        for h in &forecast.highs {
            if stated.contains(&h.day) {
                continue;
            }
            let fact = daily_fact(MetricId(METRIC.into()), SUBJECT, &subject(&h.day))
                .map_err(|e| e.to_string())?;
            let close = (fact.as_of - DAY).saturating_sub(DAILY_LEAD_SECS);
            if close <= now + CLOSE_MARGIN_SECS {
                continue;
            }
            fresh.push(sign(&relay_secret, intake_id, relay, fact, h.tenths)?);
        }
        if fresh.is_empty() {
            println!(
                "no new day to state (every forecast day is stated or too close to its close)"
            );
        } else {
            // Gas for a poster that has none (first run only).
            let bal = rec.rpc(
                "eth_getBalance",
                &[format!("\"{}\"", hex0x(&state.poster)), "\"latest\"".into()],
            )?;
            let wei =
                u128::from_str_radix(bal.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
                    .map_err(|e| e.to_string())?;
            if wei < POSTER_GAS_WEI / 4 {
                let funder = KeySigner::new(&chain, key_from_env_hex("ONYX_TRACK_FUNDER_KEY")?)?;
                funder.send_value(&state.poster, POSTER_GAS_WEI)?;
                println!("poster funded with {POSTER_GAS_WEI} wei of test ETH");
            }
            for s in &fresh {
                intake
                    .submit(&p, &r, s.clone(), now)
                    .map_err(|e| e.to_string())?;
            }
            let mut run = Run {
                submitted_at: now,
                statements: fresh.clone(),
                headers: Vec::new(),
                txs: Vec::new(),
            };
            while let Some(h) = intake.seal() {
                let seq = rec.txs.len() as u64;
                let block = poster.send_call(&rec.id.sink, &h.hash().0)?;
                let tx = find_post(&rec, block, seq, &h.hash())?;
                rec.txs.push(tx);
                run.headers.push(h.hash());
                run.txs.push(tx);
                println!(
                    "batch {seq}: header {} (reveal at {}) posted in tx {} (block {block})",
                    h.hash().to_hex(),
                    h.reveal_by,
                    hex0x(&tx.0)
                );
            }
            state.runs.push(run);
            save_state(&state_path, &state)?;
            for s in &fresh {
                if let Value::Fixed { mantissa, .. } = s.payload.value {
                    println!(
                        "stated {}: {}.{} °C",
                        day_of(s),
                        mantissa / 10,
                        (mantissa % 10).abs()
                    );
                }
            }
        }
    }

    // The publication at the final head: revealed statements only (each
    // stays sealed until its day ends), no resolution yet.
    let head = intake.sync(&rec).map_err(|e| e.to_string())?;
    let log = intake.log_at(&head);
    let book = compute(&p, &r, &log, &[], head.timestamp).map_err(|e| e.to_string())?;
    let mut publication = match std::fs::read(&out) {
        Ok(b) => borsh::from_slice::<Publication>(&b).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Publication::chained(),
        Err(e) => return Err(e.to_string()),
    };
    if publication
        .latest()
        .is_some_and(|s| s.commitment.as_of >= head.timestamp)
    {
        println!(
            "publication unchanged: the final head ({}) has not moved past the last commitment",
            head.timestamp
        );
        println!("TRACK NWS DONE");
        return Ok(());
    }
    let c = publication
        .push_with(
            &p,
            &log,
            &[],
            &book,
            Some(intake.published(&head)),
            Some(&r),
        )
        .map_err(|e| e.to_string())?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    write_publication(&out, &publication)?;
    println!(
        "publication: commitment {} ({} in all) at final block {} ({}): {} statements revealed, {} sealed until their day ends",
        c.hash().to_hex(),
        publication.steps.len(),
        head.block,
        head.timestamp,
        log.len(),
        state.runs.iter().map(|r| r.statements.len()).sum::<usize>() - log.len()
    );
    println!("TRACK NWS DONE");
    Ok(())
}

fn key_from_env_hex(var: &str) -> Result<[u8; 32], String> {
    let v = std::env::var(var)
        .map_err(|_| format!("{var} is not set (needed once, to fund the poster)"))?;
    let mut k = [0u8; 32];
    hex::decode_to_slice(v.trim().trim_start_matches("0x"), &mut k)
        .map_err(|_| format!("{var} is not a 32-byte hex key"))?;
    Ok(k)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("track-nws: {e}");
        std::process::exit(1);
    }
}
