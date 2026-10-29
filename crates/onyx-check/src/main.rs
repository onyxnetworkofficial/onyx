//! `onyx-check`: check one Onyx API answer, or a payment offer before paying it (NS8, ADR-0047).
//!
//! ```text
//! onyx-check --answer answer.json --asked '{"kind":"proof","round":1,"shard":"0x…","index":0}' \
//!            --rpc https://sepolia.base.org --chain-id 84532 --contract 0x… \
//!            [--registry 0x… [--ladder-state ~/.onyx/ladders]]
//! onyx-check --answer - --asked @asked.json --unchecked
//! onyx-check --offer offer.json --manifest deploy.json --max-amount 10000 --rpc https://…
//! ```
//! Writes one JSON report to stdout. Exit 0 verified (or payable), 1 refused,
//! 2 usage error, 3 nothing decided (your node could not be read, or your
//! settings are wrong).

use onyx_api::roots::HttpJsonRpc;
use onyx_api::x402::{check_facilitator_url, HyperPost};
use onyx_check::{
    address, check_offer, check_on_bound, check_unchecked, parse_asked, Bind, Node, UsageError,
    MAX_ANSWER_BYTES,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

const USAGE: &str = "usage: onyx-check --answer <file|-> --asked <json|@file> \
(--rpc <url> --chain-id <n> --contract <0x…> [--registry <0x…> [--ladder-state <dir>]] \
| --unchecked)\n   or: onyx-check --offer <file|-> \
--manifest <file> --max-amount <n> --rpc <url>";

/// Each node call gives up after this long.
const RPC_TIMEOUT: Duration = Duration::from_secs(20);

enum Mode {
    Answer {
        answer: String,
        asked: String,
        node: Option<Node>,
        bind: Bind,
    },
    Offer {
        offer: String,
        manifest: String,
        max_amount: u128,
        rpc: String,
    },
}

const VALUED: &[&str] = &[
    "--answer",
    "--asked",
    "--rpc",
    "--chain-id",
    "--contract",
    "--registry",
    "--ladder-state",
    "--offer",
    "--manifest",
    "--max-amount",
];

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Mode, UsageError> {
    let mut v: BTreeMap<String, String> = BTreeMap::new();
    let mut unchecked = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--unchecked" => unchecked = true,
            "-h" | "--help" => return Err(UsageError(USAGE.into())),
            k if VALUED.contains(&k) => {
                let val = it
                    .next()
                    .ok_or_else(|| UsageError(format!("{a} needs a value")))?;
                if v.insert(a.clone(), val).is_some() {
                    return Err(UsageError(format!("{a} given twice")));
                }
            }
            _ => return Err(UsageError(format!("unknown argument {a}\n{USAGE}"))),
        }
    }
    if let Some(offer) = v.remove("--offer") {
        let manifest = v
            .remove("--manifest")
            .ok_or_else(|| UsageError("--manifest is required with --offer".into()))?;
        let max = v.remove("--max-amount").ok_or_else(|| {
            UsageError(
                "--max-amount is required with --offer (your limit, in the token's smallest unit)"
                    .into(),
            )
        })?;
        let max_amount = max
            .parse::<u128>()
            .map_err(|_| UsageError(format!("--max-amount: not a whole number: {max}")))?;
        let rpc = v.remove("--rpc").ok_or_else(|| {
            UsageError(
                "--rpc is required with --offer (no offer is checked without your node)".into(),
            )
        })?;
        rpc_url(&rpc)?;
        if unchecked || !v.is_empty() {
            return Err(UsageError(format!(
                "--offer takes only --manifest, --max-amount and --rpc\n{USAGE}"
            )));
        }
        return Ok(Mode::Offer {
            offer,
            manifest,
            max_amount,
            rpc,
        });
    }
    let answer = v
        .remove("--answer")
        .ok_or_else(|| UsageError(format!("--answer is required\n{USAGE}")))?;
    let asked = v
        .remove("--asked")
        .ok_or_else(|| UsageError(format!("--asked is required\n{USAGE}")))?;
    let chain_id = v
        .remove("--chain-id")
        .map(|c| {
            c.parse::<u64>()
                .map_err(|_| UsageError(format!("--chain-id: not a number: {c}")))
        })
        .transpose()?;
    let contract = v.remove("--contract").map(|c| address(&c)).transpose()?;
    let bind = Bind {
        registry: v.remove("--registry").map(|c| address(&c)).transpose()?,
        ladder_state: v.remove("--ladder-state").map(std::path::PathBuf::from),
    };
    if bind.ladder_state.is_some() && bind.registry.is_none() {
        return Err(UsageError("--ladder-state needs --registry".into()));
    }
    if unchecked && bind.registry.is_some() {
        return Err(UsageError(
            "--registry is read on your node: not with --unchecked".into(),
        ));
    }
    let rpc = v.remove("--rpc");
    if let Some(k) = v.keys().next() {
        return Err(UsageError(format!("{k} is only for --offer\n{USAGE}")));
    }
    let node = match (rpc, chain_id, contract, unchecked) {
        (Some(rpc), Some(chain_id), Some(contract), false) => {
            rpc_url(&rpc)?;
            Some(Node {
                rpc,
                chain_id,
                contract,
            })
        }
        (None, None, None, true) => None,
        (None, None, None, false) => {
            return Err(UsageError(format!(
                "name your own chain node (--rpc, --chain-id, --contract), or pass --unchecked \
                 to skip the root read (the answer is then NOT VERIFIED ON CHAIN)\n{USAGE}"
            )))
        }
        (_, _, _, true) => return Err(UsageError("--unchecked with a node: pick one".into())),
        _ => {
            return Err(UsageError(
                "--rpc, --chain-id and --contract go together".into(),
            ))
        }
    };
    Ok(Mode::Answer {
        answer,
        asked,
        node,
        bind,
    })
}

/// HTTPS, or http only to this machine: the transport's own rule, checked
/// here so a wrong URL is a usage error, not a node that "may come back".
fn rpc_url(url: &str) -> Result<(), UsageError> {
    check_facilitator_url(url).map_err(|e| UsageError(format!("--rpc: {e}")))
}

fn read_capped(r: impl Read, what: &str) -> Result<Vec<u8>, UsageError> {
    let mut buf = Vec::new();
    r.take(MAX_ANSWER_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| UsageError(format!("{what}: {e}")))?;
    if buf.len() as u64 > MAX_ANSWER_BYTES {
        return Err(UsageError(format!(
            "{what}: larger than {MAX_ANSWER_BYTES} bytes"
        )));
    }
    Ok(buf)
}

/// JSON from a file, `-` (stdin) or, when `inline`, the text itself
/// (`@file` for a file).
fn json_arg(arg: &str, what: &str, inline: bool) -> Result<Value, UsageError> {
    let bytes = if arg == "-" {
        read_capped(std::io::stdin().lock(), what)?
    } else {
        let path = match (inline, arg.strip_prefix('@')) {
            (true, None) => {
                return serde_json::from_str(arg)
                    .map_err(|e| UsageError(format!("{what} is not JSON: {e}")))
            }
            (true, Some(p)) => p,
            (false, _) => arg,
        };
        let f = std::fs::File::open(path).map_err(|e| UsageError(format!("{what} {path}: {e}")))?;
        read_capped(f, what)?
    };
    serde_json::from_slice(&bytes).map_err(|e| UsageError(format!("{what} is not JSON: {e}")))
}

fn runtime() -> Result<tokio::runtime::Runtime, UsageError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| UsageError(format!("runtime: {e}")))
}

fn run() -> Result<(Value, i32), UsageError> {
    match parse_args(std::env::args().skip(1))? {
        Mode::Answer {
            answer,
            asked,
            node,
            bind,
        } => {
            let answer = json_arg(&answer, "answer", false)?;
            let asked = parse_asked(&json_arg(&asked, "asked", true)?)?;
            let outcome = match node {
                None => check_unchecked(&answer, &asked),
                Some(n) => {
                    let rpc = HttpJsonRpc::new(n.rpc, HyperPost::for_rpc(RPC_TIMEOUT));
                    runtime()?.block_on(check_on_bound(
                        &answer, &asked, rpc, n.chain_id, n.contract, &bind,
                    ))
                }
            };
            Ok((outcome.report(), outcome.code()))
        }
        Mode::Offer {
            offer,
            manifest,
            max_amount,
            rpc,
        } => {
            let offer = json_arg(&offer, "offer", false)?;
            let manifest = json_arg(&manifest, "manifest", false)?;
            let rpc = HttpJsonRpc::new(rpc, HyperPost::for_rpc(RPC_TIMEOUT));
            let o = runtime()?.block_on(check_offer(&offer, &manifest, max_amount, &rpc));
            Ok((o.report(), o.code()))
        }
    }
}

fn main() {
    let (report, code) = match run() {
        Ok(r) => r,
        Err(UsageError(m)) => (
            json!({"ok": false, "decided": false, "code": "usage", "error": m}),
            2,
        ),
    };
    println!("{report}");
    std::process::exit(code);
}
