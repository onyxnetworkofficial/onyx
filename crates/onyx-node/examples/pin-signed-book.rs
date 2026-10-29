//! Pin a freshly deployed `OnyxSignedBook` into a node's deployment manifest
//! (ROADMAP C10; driven by scripts/deploy-signed-book.sh, runbook
//! docs/ops/runbooks/deploy-signed-book.md). Sends nothing.
//!
//! ```text
//! ONYX_PIN_RPC=<node URL> pin-signed-book --manifest deployment.json \
//!   --broadcast contracts/broadcast/DeploySignedBook.s.sol/<chain>/run-latest.json \
//!   --artifact contracts/out/OnyxSignedBook.sol/OnyxSignedBook.json \
//!   --reviewer 0x<reviewer> [--replace]
//! ```
//!
//! Before writing `signedBook`, `signedBookCodeHash` and `signedBookReviewer`
//! it checks, on the node: the chain id is the manifest's; the code at the
//! broadcast's address is this checkout's build with only `reviewer` filled
//! in; `reviewer()` is `--reviewer`; and the reviewer is not the manifest
//! settlement's `coordinator()` (THREATS C10-1). A manifest that already pins
//! another book is refused unless `--replace`. The RPC URL is read from the
//! environment, never the command line (it can carry an API key).

use onyx_api::roots::{HttpJsonRpc, JsonRpc};
use onyx_api::signed_serve::{book_from_broadcast, check_book, code_matches_build, BookPin};
use onyx_api::x402::HyperPost;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::time::Duration;

type Res<T> = Result<T, String>;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn read_json(path: &str) -> Res<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?)
        .map_err(|e| format!("{path}: {e}"))
}

fn addr(s: &str, what: &str) -> Res<[u8; 20]> {
    hex::decode(s.trim().strip_prefix("0x").unwrap_or(s.trim()))
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{what} must be a 0x address"))
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

async fn word_address(rpc: &dyn JsonRpc, to: &[u8; 20], sig: &str) -> Res<[u8; 20]> {
    let sel = &keccak256(sig.as_bytes()).0[..4];
    let ret = rpc
        .call(
            "eth_call",
            json!([{"to": hx(to), "data": hx(sel)}, "latest"]),
        )
        .await?;
    let b = hex::decode(ret.as_str().unwrap_or_default().trim_start_matches("0x"))
        .map_err(|e| format!("{sig}: {e}"))?;
    if b.len() != 32 || b[..12].iter().any(|x| *x != 0) {
        return Err(format!("{sig}: not an address word"));
    }
    addr(&hex::encode(&b[12..]), sig)
}

async fn run(args: &[String]) -> Res<()> {
    let need = |f: &str| arg(args, f).ok_or_else(|| format!("{f} is required"));
    let manifest_path = need("--manifest")?;
    let broadcast = read_json(&need("--broadcast")?)?;
    let artifact = read_json(&need("--artifact")?)?;
    let reviewer = addr(&need("--reviewer")?, "--reviewer")?;
    let replace = args.iter().any(|a| a == "--replace");
    let url = std::env::var("ONYX_PIN_RPC").map_err(|_| "ONYX_PIN_RPC is not set")?;
    let rpc = HttpJsonRpc::new(url, HyperPost::for_rpc(Duration::from_secs(20)));

    let mut manifest = read_json(&manifest_path)?;
    let chain_id = manifest["chainId"]
        .as_u64()
        .ok_or("manifest chainId must be a number")?;
    let settlement = addr(
        manifest["settlement"].as_str().unwrap_or_default(),
        "manifest settlement",
    )?;
    let book = book_from_broadcast(&broadcast)?;
    if let Some(old) = manifest["signedBook"].as_str() {
        if addr(old, "manifest signedBook")? != book && !replace {
            return Err(format!(
                "the manifest already pins the signed book {old}; pass --replace to pin {} instead",
                hx(&book)
            ));
        }
    }
    let code = rpc
        .call("eth_getCode", json!([hx(&book), "latest"]))
        .await?;
    let code = hex::decode(code.as_str().unwrap_or_default().trim_start_matches("0x"))
        .map_err(|e| format!("code: {e}"))?;
    code_matches_build(&code, &artifact)?;
    let pin = BookPin {
        chain_id,
        book,
        code_hash: keccak256(&code),
        reviewer,
    };
    // The node's own start check: chain id, code hash, reviewer().
    check_book(&rpc, &pin).await?;
    let coordinator = word_address(&rpc, &settlement, "coordinator()").await?;
    if coordinator == reviewer {
        return Err(
            "the reviewer is the settlement's coordinator (the deploy key): refused (THREATS C10-1)"
                .into(),
        );
    }
    manifest["signedBook"] = json!(hx(&book));
    manifest["signedBookCodeHash"] = json!(Hash32(pin.code_hash.0).to_string());
    manifest["signedBookReviewer"] = json!(hx(&reviewer));
    let tmp = format!("{manifest_path}.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| format!("{tmp}: {e}"))?;
    std::fs::rename(&tmp, &manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?;
    println!("signedBook {}", hx(&book));
    println!("signedBookCodeHash {}", pin.code_hash);
    println!("signedBookReviewer {}", hx(&reviewer));
    println!("coordinator {} (not the reviewer)", hx(&coordinator));
    println!("PINNED {manifest_path}");
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args).await {
        eprintln!("pin-signed-book: {e}");
        std::process::exit(1);
    }
}
