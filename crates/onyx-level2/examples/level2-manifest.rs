//! Level-2 rounds of a manifest source (contributor loop CL3, ADR-0053): the shared runner with
//! a reviewed `sources/<id>/manifest.json` instead of an adapter. No per-source code.
//!
//! `cargo run -p onyx-level2 --example level2-manifest -- <mode> --manifest FILE [--repo DIR] ...`
//!
//! The manifest is loaded with every rule (`onyx_level2::manifest::load`, the terms table of
//! `DIR/docs/ops/data-sources.md`, default the current directory); a manifest that does not load
//! runs nothing. Modes:
//! - `coordinator`: the live coordinator (`onyx_level2::live::coordinator`, flags as
//!   `adapters/source-fiscaldata/examples/level2-coordinator.rs`). Before any draw, a metric
//!   the registry already holds must hold exactly this manifest's template (THREATS CL3-8);
//!   an unregistered one is registered by the run (`--owner`).
//! - `observer`: one independent observer (`onyx_level2::live::observer`, flags as the
//!   Treasury's `level2-observer`); it fetches the source itself through `onyx_fetch`.
//! - `check`: the client check of a served round (`onyx_level2::live::client_check`).
//! - `demo`: the local end-to-end demo. A local fixture server answers the manifest's own URL
//!   with its recording (`--body FILE`, default the manifest's `recording`); five observers fetch
//!   it through `onyx_fetch` (every transport rule; only the last hop is the loopback fixture);
//!   the round runs through the shared rehearsal (`onyx_level2::rehearse::main_for`, flags as
//!   `level2-round`: `--book`, `--store`, `--new-store`, `--out`, and `--rpc --registry
//!   --coordinator --owner` for a local anvil). `--record-date` defaults to the recording's
//!   date, `--window-start` to the start of the first day its lag allows.
//!   `--pool FILE` draws the simulated observers from that snapshot of rehearsal keys (a recheck
//!   plan's pool) instead of the default nine.
//! - `rehearsal-pool --size N`: the first N rehearsal observers (public test keys) as a pool file.
//! - `recheck-plan`, `recheck-resolve`: the recheck runner of correction markets
//!   (`onyx_level2::corrections`, CL7): plan a value's recheck round from `OnyxCorrections`,
//!   then resolve its markets from the recheck's results store.
//!
//! `--recheck AFTER_DAYS:SPAN_DAYS` runs every other mode on the manifest's paired recheck
//! (`ManifestSource::recheck`, ADR-0057 item 2): the coordinator, observers and client of a
//! recheck round use it with the plan's `--pool` and `--window-start`.

use onyx_core::feed::day_number;
use onyx_fetch::fixture::{FixtureServer, Reply};
use onyx_level2::manifest::ManifestSource;
use onyx_level2::rehearse::{main_for, Recorded};
use onyx_level2::{Page, Source};
use std::path::{Path, PathBuf};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32, String> {
    let mode = args.get(1).cloned().unwrap_or_default();
    let path = PathBuf::from(flag(args, "--manifest").ok_or("--manifest FILE is required")?);
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    let src = ManifestSource::from_file(&path, &repo).map_err(|e| e.to_string())?;
    // CL7: the runner's commands take the value's own source and read `--recheck` themselves.
    if matches!(mode.as_str(), "recheck-plan" | "recheck-resolve") {
        return onyx_level2::corrections::run(&src, args).map(|()| 0);
    }
    let src = match flag(args, "--recheck") {
        Some(v) => {
            let (a, s) = v.split_once(':').ok_or("--recheck AFTER_DAYS:SPAN_DAYS")?;
            let after = a.parse().map_err(|_| "--recheck: AFTER_DAYS is a number")?;
            let span = s.parse().map_err(|_| "--recheck: SPAN_DAYS is a number")?;
            src.recheck(after, span).map_err(|e| e.to_string())?
        }
        None => src,
    };
    match mode.as_str() {
        "coordinator" => {
            check_pin(&src, args)?;
            onyx_level2::live::coordinator(&src, args).map(|()| 0)
        }
        "observer" => Ok(onyx_level2::live::observer(&src, args)),
        "check" => onyx_level2::live::client_check(&src, args).map(|()| 0),
        "demo" => demo(&src, &path, args).map(|()| 0),
        // CL7 rehearsals: the first `--size N` rehearsal observers (public test keys, local only)
        // as a pool file, the snapshot a recheck plan filters.
        "rehearsal-pool" => {
            let n: u8 = flag(args, "--size")
                .unwrap_or_else(|| onyx_level2::POOL_SIZE.to_string())
                .parse()
                .map_err(|_| "--size N (at most 32)")?;
            let pool =
                onyx_level2::corrections::pool_json(&onyx_level2::rehearsal_pool_of(&src, n))?;
            println!("{}", serde_json::to_string_pretty(&pool).map_err(|e| e.to_string())?);
            Ok(0)
        }
        "--pin" | "pin" => {
            let t = src.template();
            println!("{}={}", t.metric.0, t.id());
            Ok(0)
        }
        _ => Err("usage: level2-manifest <coordinator|observer|check|demo|pin|rehearsal-pool|recheck-plan|recheck-resolve> --manifest FILE [--repo DIR] [--recheck A:S] ...".into()),
    }
}

/// Before any draw: when the registry already holds the metric, its template must be exactly
/// this manifest's (THREATS CL3-8); when it does not, the coordinator registers this one
/// (`--owner`), and the registry is append-only.
fn check_pin(src: &ManifestSource, args: &[String]) -> Result<(), String> {
    use onyx_coordinator::cast_chain::CastChain;
    let Some(cast) = CastChain::from_args(args)? else {
        return Ok(());
    };
    let t = src.template();
    let ret = cast.eth_call(&onyx_api::observations::registry_metric_calldata(&t), None)?;
    let registered = ret.len() == 96 && ret[64..96].iter().any(|b| *b != 0);
    if registered {
        src.check_registered(&ret)?;
        eprintln!(
            "{}: the registry holds this manifest's template {}",
            t.metric.0,
            t.id()
        );
    } else {
        eprintln!(
            "{}: not registered yet; this run registers template {}",
            t.metric.0,
            t.id()
        );
    }
    Ok(())
}

fn path_of(url: &str) -> Result<String, String> {
    let t = onyx_fetch::check_target(url).map_err(|e| e.to_string())?;
    Ok(t.path_and_query)
}

fn demo(src: &ManifestSource, manifest: &Path, args: &[String]) -> Result<(), String> {
    let m = src.manifest();
    let dir = manifest.parent().unwrap_or(Path::new("."));
    let rec = dir.join(&m.recording);
    let body_path = flag(args, "--body").map_or(rec.clone(), PathBuf::from);
    let body = std::fs::read(&body_path).map_err(|e| format!("{}: {e}", body_path.display()))?;
    let day = match flag(args, "--record-date") {
        Some(d) => d,
        None => rec
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| day_number(s).is_some())
            .ok_or("--record-date YYYY-MM-DD (the recording is not named by a date)")?
            .to_string(),
    };
    let window_start = match flag(args, "--window-start") {
        Some(w) => w.parse().map_err(|_| "--window-start UNIX")?,
        None => {
            let d = day_number(&day).ok_or("bad date")?;
            (1..=400)
                .map(|k| u64::try_from(d + k).unwrap_or(0) * 86_400)
                .find(|w| src.spec(&day, *w).is_ok())
                .ok_or("no window observes this date")?
        }
    };
    let spec = src.spec(&day, window_start)?;
    let server = FixtureServer::start(vec![(path_of(&spec.url)?, Reply::json(body.clone()))])
        .map_err(|e| e.to_string())?;
    let fetch = server.fetch(src.fetch_limits());
    let n = usize::from(spec.min_observers);
    let mut pages = Vec::with_capacity(n);
    for i in 0..n {
        let got = fetch.get_blocking(&spec.url);
        if let Err(e) = &got {
            eprintln!("observer {i}: no answer: {e}");
        }
        pages.push(Page {
            fetched_unix: window_start + 10 + 30 * u64::try_from(i).unwrap_or(0),
            body: got.ok(),
        });
    }
    eprintln!(
        "{} observers fetched {} through onyx_fetch from the local fixture {}",
        n,
        spec.url,
        server.addr()
    );
    main_for(
        src,
        Recorded {
            day,
            window_start,
            http_dates: vec!["local fixture".into(); n],
            pages,
            body,
            draw: None,
        },
        args,
    )
}
