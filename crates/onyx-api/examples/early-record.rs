//! The early-contributor record tool (ADR-0065, ROADMAP S26; runbook
//! docs/ops/early-record.md). A thin shell over `onyx_api::early_chain`.
//!
//! ```sh
//! export ONYX_EARLY_RPC=...   # the node URL (it may hold a provider key)
//! cargo run -p onyx-api --features http-client --example early-record -- build \
//!   --contracts deploy/early-record/contracts.json \
//!   --exclusions deploy/early-record/exclusions.json --state STATE_DIR --out VERSION_DIR \
//!   [--prev PREVIOUS_VERSION_DIR] [--day YYYY-MM-DD]
//! cargo run -p onyx-api --features http-client --example early-record -- verify \
//!   --contracts deploy/early-record/contracts.json \
//!   --exclusions deploy/early-record/exclusions.json --dir VERSION_DIR [--state-before FILE]
//! ```
//!
//! `build` writes the next complete UTC day into the new folder (`data.csv`,
//! `schema.json`, `day.json`, `exclusions/` when they change), or nothing when
//! no new day is final yet; it never builds a built folder again. `verify`
//! rebuilds that folder's day from the chain and compares its files, the
//! state hash in `day.json` and the shard roots of the built versions (the
//! record's and the exclusions'). Prints one JSON
//! line; exit 0 on success, 1 when the run stopped (run `build` again: it goes
//! on from the kept cursor), 2 on bad arguments. The node URL comes from
//! `ONYX_EARLY_RPC`; `--rpc URL` still works but anyone on the machine can
//! read it in the process list, so the tool warns. The URL is never printed,
//! and an unknown argument is named by its flag only.

use onyx_api::early::{day_text, parse_day};
use onyx_api::early_chain::{build, verify, BuildOpts, Built, Reader};
use onyx_api::roots::HttpJsonRpc;
use onyx_api::x402::HyperPost;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "usage: ONYX_EARLY_RPC=URL early-record build --contracts FILE --exclusions FILE --state DIR --out DIR [--prev DIR] [--day YYYY-MM-DD]\n       ONYX_EARLY_RPC=URL early-record verify --contracts FILE --exclusions FILE --dir DIR [--state-before FILE]";

/// The node URL's environment variable.
const RPC_ENV: &str = "ONYX_EARLY_RPC";

fn args(rest: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let mut m = BTreeMap::new();
    let mut it = rest.iter();
    while let Some(k) = it.next() {
        // Name an argument by its flag only: `--rpc=URL` must not print URL.
        let flag = k.split('=').next().unwrap_or_default();
        let name = k
            .strip_prefix("--")
            .filter(|n| allowed.contains(n))
            .ok_or_else(|| format!("unknown argument {flag}"))?;
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if m.insert(name.to_string(), v.clone()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok(m)
}

fn req(m: &BTreeMap<String, String>, k: &str) -> Result<String, String> {
    m.get(k)
        .cloned()
        .ok_or_else(|| format!("--{k} is required"))
}

async fn run(argv: &[String]) -> Result<serde_json::Value, (i32, String)> {
    let usage = |e: String| (2, format!("{e}\n{USAGE}"));
    let (cmd, rest) = argv
        .split_first()
        .ok_or_else(|| usage("no command".into()))?;
    let rpc_of = |m: &BTreeMap<String, String>| -> Result<HttpJsonRpc<HyperPost>, (i32, String)> {
        let url = match (
            std::env::var(RPC_ENV).ok().filter(|u| !u.trim().is_empty()),
            m.get("rpc"),
        ) {
            (Some(u), _) => u,
            (None, Some(u)) => {
                eprintln!(
                    "early-record: warning: --rpc is visible to every user in the process list; set {RPC_ENV} instead"
                );
                u.clone()
            }
            (None, None) => return Err(usage(format!("set {RPC_ENV} to the node URL"))),
        };
        Ok(HttpJsonRpc::new(
            url,
            HyperPost::for_rpc(Duration::from_secs(20)),
        ))
    };
    match cmd.as_str() {
        "build" => {
            let m = args(
                rest,
                &[
                    "rpc",
                    "contracts",
                    "exclusions",
                    "state",
                    "out",
                    "prev",
                    "day",
                ],
            )
            .map_err(usage)?;
            let day = m
                .get("day")
                .map(|d| parse_day(d).ok_or_else(|| usage(format!("--day {d}: not YYYY-MM-DD"))))
                .transpose()?;
            let o = BuildOpts {
                contracts: PathBuf::from(req(&m, "contracts").map_err(usage)?),
                exclusions: PathBuf::from(req(&m, "exclusions").map_err(usage)?),
                state: PathBuf::from(req(&m, "state").map_err(usage)?),
                out: PathBuf::from(req(&m, "out").map_err(usage)?),
                prev: m.get("prev").map(PathBuf::from),
                day,
            };
            let rpc = rpc_of(&m)?;
            let r = Reader::standard(&rpc);
            match build(&r, &o).await.map_err(|e| (1, e.to_string()))? {
                Built::Day {
                    day,
                    rows,
                    skipped,
                    empty_days,
                } => {
                    for d in &empty_days {
                        eprintln!(
                            "early-record: {} has no block at all (a halted chain?): no version for it",
                            day_text(*d)
                        );
                    }
                    Ok(json!({
                        "built": day_text(day),
                        "rows": rows,
                        "out": o.out.display().to_string(),
                        "skipped_publisher_keys": skipped.iter().map(|x| format!("{} ({})", x.address, x.source)).collect::<Vec<_>>(),
                        "days_without_blocks": empty_days.iter().map(|d| day_text(*d)).collect::<Vec<_>>(),
                    }))
                }
                Built::AlreadyBuilt(day) => Ok(json!({"already_built": day_text(day)})),
                Built::NothingNew => Ok(
                    json!({"nothing_new": "no new complete day at or below the finalized block"}),
                ),
            }
        }
        "verify" => {
            let m = args(
                rest,
                &["rpc", "contracts", "exclusions", "dir", "state-before"],
            )
            .map_err(usage)?;
            let rpc = rpc_of(&m)?;
            let r = Reader::standard(&rpc);
            let before = m.get("state-before").map(PathBuf::from);
            let v = verify(
                &r,
                &PathBuf::from(req(&m, "contracts").map_err(usage)?),
                &PathBuf::from(req(&m, "exclusions").map_err(usage)?),
                &PathBuf::from(req(&m, "dir").map_err(usage)?),
                before.as_deref(),
            )
            .await
            .map_err(|e| (1, e.to_string()))?;
            Ok(
                json!({"verified": day_text(v.day), "rows": v.rows, "shard_roots": v.shards, "exclusion_shard_roots": v.exclusion_shards}),
            )
        }
        c => Err(usage(format!("unknown command {c}"))),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match run(&argv).await {
        Ok(v) => println!("{v}"),
        Err((code, e)) => {
            eprintln!("early-record: {e}");
            std::process::exit(code);
        }
    }
}
