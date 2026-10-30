//! Aligned multi-venue feeds (contributor loop CL8, ADR-0058): the local end-to-end demo and the
//! client check, over a composite of bucketed venue manifests. No per-venue code.
//!
//! `cargo run -p onyx-level2 --example level2-aligned -- <mode> --composite FILE --repo DIR ...`
//!
//! - `pin`: prints `metric=template` of each venue feed, one per line.
//! - `demo --work DIR [--bucket-start N] [--rpc URL --registry ADDR --coordinator ADDR --owner
//!   ADDR --cast PATH]`: one loopback fake venue per venue manifest answers that venue's own URL
//!   with its recording (the scenario); five observers per venue fetch it through `onyx_fetch`
//!   (every transport rule; only the last hop is the loopback fixture); each venue's round runs
//!   through the shared rehearsal (`onyx_level2::rehearse::main_for`), registered on the chain
//!   and, with `--rpc`, recorded on it (`--record-rounds`); then the aligned answer is computed
//!   from the venue facts (values, the agreeing reveals' times, the venue's own bucket time read
//!   from its page, the block and time of `ObservationRecorded`) and written to
//!   `DIR/aligned.json`. `--bucket-start` defaults to the recordings' bucket.
//! - `check --work DIR [--answer FILE] [chain flags]`: the client check: recompute the answer
//!   from the venue summaries and the chain, and refuse a served answer that differs or leaves
//!   out a listed venue; then show that a dropped venue, a cleared flag and a moved value are
//!   refused.

use onyx_coordinator::cast_chain::CastChain;
use onyx_coordinator::observe::OnchainObservationCheck;
use onyx_core::aligned::VenueInput;
use onyx_core::onchain::{metric_key, subject_key};
use onyx_fetch::fixture::{FixtureServer, Reply};
use onyx_level2::aligned::{
    answer_json, check_answer_json, first_window, lapsed, load_composite, venue_input, venue_round,
    CompositeManifest, OnChain, RecordedAt, VenueRound,
};
use onyx_level2::manifest::ManifestSource;
use onyx_level2::rehearse::{main_for, Recorded};
use onyx_level2::{Page, Source};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    let mode = args.get(1).cloned().unwrap_or_default();
    let path = PathBuf::from(flag(args, "--composite").ok_or("--composite FILE is required")?);
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    let cm = load_composite(&path, &repo)?;
    match mode.as_str() {
        "pin" => {
            for v in &cm.venues {
                let t = v.template();
                println!("{}={}", t.metric.0, t.id());
            }
            Ok(())
        }
        "demo" => demo(&cm, args),
        "check" => check(&cm, args),
        _ => Err("usage: level2-aligned <pin|demo|check> --composite FILE [--repo DIR] ...".into()),
    }
}

fn work_dir(args: &[String]) -> Result<PathBuf, String> {
    let w = PathBuf::from(flag(args, "--work").ok_or("--work DIR is required")?);
    std::fs::create_dir_all(&w).map_err(|e| format!("{}: {e}", w.display()))?;
    Ok(w)
}

/// The bucket every venue recording names (the file stem), or `--bucket-start`.
fn bucket_start(cm: &CompositeManifest, args: &[String]) -> Result<u64, String> {
    if let Some(b) = flag(args, "--bucket-start") {
        return b.parse().map_err(|_| "--bucket-start UNIX".into());
    }
    let mut starts: Vec<u64> = cm
        .venues
        .iter()
        .filter_map(|v| {
            Path::new(&v.manifest().recording)
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(onyx_core::feed::canonical_u64)
        })
        .collect();
    starts.dedup();
    match starts.as_slice() {
        [one] => Ok(*one),
        _ => Err("the recordings name different buckets: pass --bucket-start".into()),
    }
}

fn venue_dir(work: &Path, src: &ManifestSource) -> PathBuf {
    work.join(&src.manifest().id)
}

fn demo(cm: &CompositeManifest, args: &[String]) -> Result<(), String> {
    let work = work_dir(args)?;
    let start = bucket_start(cm, args)?;
    let chained = flag(args, "--rpc").is_some();
    for src in &cm.venues {
        let m = src.manifest();
        let window_start = first_window(src, start)?;
        let key = start.to_string();
        let spec = src.spec(&key, window_start)?;
        let rec_path = venue_recording(src, args)?;
        let body = std::fs::read(&rec_path).map_err(|e| format!("{}: {e}", rec_path.display()))?;
        let t = onyx_fetch::check_target(&spec.url).map_err(|e| e.to_string())?;
        // One loopback fake venue per venue manifest, answering only that venue's own URL.
        let server = FixtureServer::start(vec![(t.path_and_query, Reply::json(body.clone()))])
            .map_err(|e| e.to_string())?;
        let fetch = server.fetch(src.fetch_limits());
        let n = usize::from(spec.min_observers);
        let mut pages = Vec::with_capacity(n);
        for i in 0..n {
            let got = fetch.get_blocking(&spec.url);
            if let Err(e) = &got {
                eprintln!("{}: observer {i}: no answer: {e}", m.id);
            }
            pages.push(Page {
                fetched_unix: window_start + 10 + 20 * u64::try_from(i).unwrap_or(0),
                body: got.ok(),
            });
        }
        eprintln!(
            "{}: {n} observers fetched {} from the loopback fake venue {}",
            m.id,
            spec.url,
            server.addr()
        );
        let dir = venue_dir(&work, src);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let p = |f: &str| dir.join(f).display().to_string();
        let mut a: Vec<String> = vec![
            "--book".into(),
            p("book.json"),
            "--store".into(),
            p("store.json"),
            "--new-store".into(),
            "--out".into(),
            p("summary.json"),
        ];
        for f in ["--rpc", "--registry", "--coordinator", "--owner", "--cast"] {
            if let Some(v) = flag(args, f) {
                a.extend([f.to_string(), v]);
            }
        }
        if chained {
            a.push("--record-rounds".into());
        }
        main_for(
            src,
            Recorded {
                day: key,
                window_start,
                http_dates: vec!["loopback fake venue".into(); n],
                pages,
                body,
                draw: None,
            },
            &a,
        )
        .map_err(|e| format!("{}: {e}", m.id))?;
    }
    let inputs = inputs_of(cm, &work, start, args, None)?;
    let answer = onyx_level2::aligned::align(cm, start, &inputs)?;
    let j = answer_json(cm, &answer);
    let out = work.join("aligned.json");
    let text = serde_json::to_string_pretty(&j).map_err(|e| e.to_string())?;
    std::fs::write(&out, format!("{text}\n")).map_err(|e| e.to_string())?;
    println!("{text}");
    eprintln!("wrote {}", out.display());
    Ok(())
}

/// The demo's per-venue recording (`--recording-dir DIR` replaces the manifests' own: a file
/// `DIR/<venue id>.json`).
fn venue_recording(src: &ManifestSource, args: &[String]) -> Result<PathBuf, String> {
    let m = src.manifest();
    if let Some(d) = flag(args, "--recording-dir") {
        return Ok(PathBuf::from(d).join(format!("{}.json", m.id)));
    }
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    Ok(repo
        .join(onyx_level2::manifest::SOURCES_DIR)
        .join(&m.id)
        .join(&m.recording))
}

fn u64_word(n: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&n.to_be_bytes());
    w
}

/// What the chain holds for a venue's fact (ADR-0058 item 4; review CL8-R1-2): the result of
/// `OnyxRegistry.observationOf(metric, subject, 0)` (a dated fact's key ignores the window end),
/// the block and time of its record, and whether `observationLevel` is 2 for exactly `round`.
fn chain_view(
    cast: &CastChain,
    src: &ManifestSource,
    start: u64,
    round: Option<&OnchainObservationCheck>,
) -> Result<OnChain, String> {
    let t = src.template();
    let subject = src
        .spec(&start.to_string(), first_window(src, start)?)?
        .subject;
    let mut of = onyx_core::keccak256(b"observationOf(bytes32,bytes32,uint64)").0[..4].to_vec();
    of.extend_from_slice(&metric_key(&t.metric).0);
    of.extend_from_slice(&subject_key(&subject).0);
    of.extend_from_slice(&u64_word(0));
    let ret = cast.eth_call(&of, None)?;
    if ret.len() != 7 * 32 {
        return Err(format!("observationOf returned {} bytes", ret.len()));
    }
    if ret[5 * 32..6 * 32].iter().all(|b| *b == 0) {
        return Ok(OnChain::Nothing);
    }
    let block = u64::from_be_bytes(ret[7 * 32 - 8..].try_into().map_err(|_| "recordedAt")?);
    let out = cast.run(&[
        "block".into(),
        block.to_string(),
        "--field".into(),
        "timestamp".into(),
    ])?;
    let time = out
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(format!("block {block}: timestamp {out}"))?;
    let level2 = match round {
        None => false,
        Some(rec) => {
            let mut q =
                onyx_core::keccak256(b"observationLevel(bytes32,bytes32,uint64,bytes32,bytes32)").0
                    [..4]
                    .to_vec();
            q.extend_from_slice(&rec.metric_key.0);
            q.extend_from_slice(&rec.subject_key.0);
            q.extend_from_slice(&u64_word(rec.window_end));
            q.extend_from_slice(&rec.spec_id.0);
            q.extend_from_slice(&rec.result.0);
            let r = cast.eth_call(&q, None)?;
            r.len() == 32 && r[31] == 2
        }
    };
    Ok(OnChain::Recorded {
        level2,
        at: RecordedAt { block, time },
    })
}

type Inputs = Vec<(String, VenueInput)>;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every listed venue's input for the bucket, as a client builds it: the venue's results store
/// (every round checked against the manifest's template), its page from the round summary when
/// there is one (hash-checked against the agreeing reveals), and the chain's record. `hide` drops
/// one venue's store and page, as an operator hiding it would (the attack the check refuses).
fn inputs_of(
    cm: &CompositeManifest,
    work: &Path,
    start: u64,
    args: &[String],
    hide: Option<&str>,
) -> Result<Inputs, String> {
    let cast = CastChain::from_args(args)?;
    let mut inputs = vec![];
    for src in &cm.venues {
        let dir = venue_dir(work, src);
        let hidden = hide == Some(src.manifest().id.as_str());
        let store = std::fs::read_to_string(dir.join("store.json"))
            .ok()
            .filter(|_| !hidden);
        let summary: Option<Json> = std::fs::read(dir.join("summary.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(|_| !hidden);
        let page = summary
            .as_ref()
            .and_then(|s| s["response_body"].as_str().map(|p| p.as_bytes().to_vec()));
        let now = unix_now();
        let round = venue_round(src, start, store.as_deref(), page.as_deref(), now)?;
        let on_chain = match (&cast, &round) {
            (Some(c), VenueRound::Value { onchain, .. }) => {
                chain_view(c, src, start, Some(onchain))?
            }
            (Some(c), _) => chain_view(c, src, start, None)?,
            (None, _) => OnChain::Nothing,
        };
        let input = venue_input(&round, on_chain, lapsed(src, start, now)?)
            .map_err(|e| format!("{}: {e}", src.manifest().id))?;
        inputs.push((src.template().metric.0, input));
    }
    Ok(inputs)
}

fn check(cm: &CompositeManifest, args: &[String]) -> Result<(), String> {
    let work = work_dir(args)?;
    let answer_path =
        flag(args, "--answer").map_or_else(|| work.join("aligned.json"), PathBuf::from);
    let served: Json = serde_json::from_slice(
        &std::fs::read(&answer_path).map_err(|e| format!("{}: {e}", answer_path.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let start = served["bucket"]["start"]
        .as_u64()
        .ok_or("the answer names no bucket")?;
    let inputs = inputs_of(cm, &work, start, args, None)?;
    check_answer_json(cm, &inputs, &served)?;
    println!(
        "client check: the served answer equals the one recomputed from the {} venue facts (value {}, disagreement {})",
        inputs.len(),
        served["value"],
        served["disagreement"]
    );
    // Forged answers the client refuses (THREATS CL8-4, CL8-2).
    let mut attacks: Vec<(&str, Json)> = vec![];
    let mut dropped = served.clone();
    if let Some(vs) = dropped["venues"].as_array_mut() {
        if let Some(i) = vs.iter().position(|v| v["included"] == true) {
            let far = vs
                .iter()
                .enumerate()
                .filter(|(_, v)| v["included"] == true)
                .max_by_key(|(_, v)| {
                    v["value"]
                        .as_str()
                        .and_then(|x| onyx_json::mantissa(x, cm.composite.scale).ok())
                        .unwrap_or(0)
                })
                .map_or(i, |(j, _)| j);
            vs.remove(far);
        }
    }
    attacks.push(("the disagreeing venue left out", dropped));
    let mut flag_off = served.clone();
    flag_off["disagreement"] = Json::Bool(false);
    attacks.push(("the disagreement flag cleared", flag_off));
    let mut moved = served.clone();
    moved["value"] = Json::String("1.00".into());
    attacks.push(("the value moved", moved));
    let mut hidden = served.clone();
    if let Some(v) = hidden["venues"].as_array_mut().and_then(|v| v.first_mut()) {
        v["included"] = Json::Bool(false);
        v["reason"] = Json::String("unavailable".into());
    }
    attacks.push(("an included venue relabelled unavailable", hidden));
    let mut clock = served.clone();
    if let Some(v) = clock["venues"].as_array_mut().and_then(|v| v.first_mut()) {
        v["observedFrom"] = Json::from(v["observedFrom"].as_u64().unwrap_or(0) + 1);
    }
    attacks.push(("an observer clock moved", clock));
    for (what, forged) in attacks {
        match check_answer_json(cm, &inputs, &forged) {
            Ok(()) => return Err(format!("client check ACCEPTED a forged answer: {what}")),
            Err(e) => println!("refused ({what}): {e}"),
        }
    }
    // The venue list is pinned: a venue of the composite with no input at all is an error.
    let short: Inputs = inputs.iter().skip(1).cloned().collect();
    if onyx_level2::aligned::align(cm, start, &short).is_ok() {
        return Err("an alignment without a listed venue was accepted".into());
    }
    println!("refused (a listed venue missing from the inputs)");
    // Review CL8-R1-2: the operator serves no store for a recorded venue; the chain says the
    // venue's fact is recorded, so the client refuses rather than read it as not observed.
    if CastChain::from_args(args)?.is_some() {
        for src in &cm.venues {
            let id = src.manifest().id.as_str();
            let recorded = matches!(
                inputs
                    .iter()
                    .find(|(m, _)| *m == src.template().metric.0)
                    .map(|(_, i)| i),
                Some(VenueInput::Recorded(_))
            );
            if !recorded {
                continue;
            }
            match inputs_of(cm, &work, start, args, Some(id)) {
                Ok(_) => return Err(format!("a hidden recorded venue ({id}) was accepted")),
                Err(e) => println!("refused (the store of a recorded venue hidden): {e}"),
            }
            break;
        }
    }
    Ok(())
}
