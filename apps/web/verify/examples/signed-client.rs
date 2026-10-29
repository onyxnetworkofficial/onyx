//! ROADMAP C61: a client checks a `/v1/signed` answer against its own read
//! of `OnyxSignedBook` at its own RPC's finalized block
//! (`onyx_web_verify::signed_view::check`, the code the browser runs), on the
//! local anvil of `scripts/level3-book.sh` (step 7).
//!
//! ```text
//! signed-client --rpc http://127.0.0.1:PORT --node http://127.0.0.1:PORT \
//!   --fact 0x.. --chain-id 31337 --book 0x.. --code-hash 0x.. \
//!   [--clock-stand-in SECS] [--export FILE]
//! ```
//!
//! It fetches the body from the node, reads chain id, the finalized block and
//! (at that block's hash) the book's code, `settleable` and `factView` from
//! `--rpc`, and requires the check to pass; then a body whose value was
//! forged (hashes recomputed, the source's signature kept) must be refused,
//! and so must the honest body against a pin naming another book or code.
//! `--clock-stand-in`: the rehearsal moved the anvil's clock with
//! `evm_increaseTime`, so the client's clock is the host's plus this many
//! seconds (as the rehearsal's node log stands in for host time).
//! `--export FILE` writes the body and the reads as the website's recorded
//! rehearsal (apps/web/static/level3/signed-rehearsal.json) and prints the
//! pin constants for `signed_view::rehearsal`.
//!
//! Loopback only; curl with User-Agent onyx-dev. Prints
//! `SIGNED CLIENT CHECK PASSED`; any failure exits non-zero.

use onyx_core::{keccak256, Hash32};
use onyx_web_verify::signed_view::{
    calls, check, decode_reads, forge_value, reads_digest, BookPin, SERVABLE_AFTER_SECS,
};
use serde_json::{json, Value};
use std::process::Command;

type Res<T> = Result<T, String>;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn need(args: &[String], flag: &str) -> Res<String> {
    arg(args, flag).ok_or_else(|| format!("{flag} is required"))
}

fn loopback(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:")
}

fn curl(extra: &[&str], url: &str) -> Res<Value> {
    let out = Command::new("curl")
        .args(["-sS", "--max-time", "30", "-A", "onyx-dev"])
        .args(extra)
        .arg(url)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("not JSON from {url}: {e}"))
}

fn rpc(url: &str, method: &str, params: Value) -> Res<Value> {
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let v = curl(
        &[
            "-H",
            "content-type: application/json",
            "--data-binary",
            &req,
        ],
        url,
    )?;
    if !v["error"].is_null() {
        return Err(format!("{method}: {}", v["error"]));
    }
    Ok(v["result"].clone())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn h32(s: &str) -> Res<Hash32> {
    let b = hex::decode(s.trim_start_matches("0x")).map_err(|e| format!("{s}: {e}"))?;
    Ok(Hash32(
        b.try_into().map_err(|_| format!("{s}: not 32 bytes"))?,
    ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => println!("SIGNED CLIENT CHECK PASSED"),
        Err(e) => {
            eprintln!("signed-client: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Res<()> {
    let (rpc_url, node) = (need(args, "--rpc")?, need(args, "--node")?);
    if !loopback(&rpc_url) || !loopback(&node) {
        return Err("rehearsal only: --rpc and --node must be loopback URLs".into());
    }
    let fact = h32(&need(args, "--fact")?)?;
    let book: [u8; 20] = hex::decode(need(args, "--book")?.trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("--book: not an address")?;
    let pin = BookPin {
        chain_id: need(args, "--chain-id")?
            .parse()
            .map_err(|_| "--chain-id: not a number")?,
        book,
        code_hash: h32(&need(args, "--code-hash")?)?,
    };
    let stand_in: u64 = arg(args, "--clock-stand-in")
        .map(|s| s.parse().map_err(|_| "--clock-stand-in: not a number"))
        .transpose()?
        .unwrap_or(0);

    let body = curl(&[], &format!("{node}/v1/signed/{fact}"))?;
    println!("  body: level {} value {}", body["level"], body["value"]);
    // The client's own reads: chain id, finalized block, then every read at
    // that block's hash (EIP-1898).
    let chain_id = rpc(&rpc_url, "eth_chainId", json!([]))?;
    let block = rpc(
        &rpc_url,
        "eth_getBlockByNumber",
        json!(["finalized", false]),
    )?;
    // EIP-1898: the block must be on the RPC's canonical chain (review C61-R1-7).
    let at = json!({"blockHash": block["hash"], "requireCanonical": true});
    let c = calls(&pin, &fact);
    let code = rpc(&rpc_url, "eth_getCode", json!([c["to"], at]))?;
    let settleable = rpc(
        &rpc_url,
        "eth_call",
        json!([{"to": c["to"], "data": c["settleable"]}, at]),
    )?;
    let fact_view = rpc(
        &rpc_url,
        "eth_call",
        json!([{"to": c["to"], "data": c["factView"]}, at]),
    )?;
    let finalized = json!({
        "number": block["number"],
        "hash": block["hash"],
        "timestamp": block["timestamp"],
    });
    let reads_json = json!({
        "chainId": chain_id,
        "finalized": finalized,
        "code": code,
        "settleable": settleable,
        "factView": fact_view,
    });
    let reads = decode_reads(&reads_json)?;
    let host = now_unix();
    let now = host + stand_in;
    let checked =
        check(&body, &pin, &reads, now).map_err(|p| format!("REFUSED: {}", p.join("; ")))?;
    println!(
        "  ok: checked against settleable at the client's finalized block {} (accepted {}, margin {} s)",
        reads.number, checked.accepted_at, SERVABLE_AFTER_SECS
    );
    let forged = forge_value(&body)?;
    match check(&forged, &pin, &reads, now) {
        Ok(_) => return Err("a forged value passed the client check".into()),
        Err(p) => println!("  ok: a forged value is refused ({})", p.join("; ")),
    }
    let mut other = pin;
    other.code_hash = keccak256(b"another build");
    if check(&body, &other, &reads, now).is_ok() {
        return Err("another code hash passed the client check".into());
    }
    other = pin;
    other.book[19] ^= 1;
    if check(&body, &other, &reads, now).is_ok() {
        return Err("another book passed the client check".into());
    }
    println!("  ok: a pin naming another code hash or book is refused");
    if let Some(path) = arg(args, "--export") {
        let digest = reads_digest(&reads);
        let doc = json!({
            "kind": "onyx-level3-rehearsal",
            "version": 1,
            "rehearsal": {
                "what": "A level-3 answer from the ROADMAP C10 rehearsal on a local anvil (chain 31337) that no longer runs: test keys, an invented test source, no live deployment. Recorded with the chain answers the client read at the same moment.",
                "script": "scripts/level3-book.sh local (apps/web/verify/examples/signed-client.rs --export)",
                "chain": "local anvil, chain 31337",
                "recordedUnix": host,
                "clockStandInSecs": stand_in,
                "readsDigest": digest.to_string(),
            },
            "body": body,
            "reads": reads_json,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
        std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
        println!("  exported {path}");
        println!("PIN BOOK {}", c["to"].as_str().unwrap_or(""));
        println!("PIN CODE_HASH {}", pin.code_hash);
        println!("PIN READS_DIGEST {digest}");
        println!("PIN RECORDED_UNIX {host}");
        println!("PIN CLOCK_STAND_IN_SECS {stand_in}");
    }
    Ok(())
}
