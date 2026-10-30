//! The signed book reviewer's live action for a bulk publisher (contributor loop CL5, ADR-0059;
//! docs/ops/bulk-publish.md "For the operator"): register a publisher's key in `OnyxSignedBook`
//! with the reviewer key read from a 0600 file, signed in this process (`KeySigner`: the key is
//! never on a command line, in the environment or printed).
//!
//! `cargo run -p onyx-level2 --example book-reviewer -- register-key --rpc URL --book ADDR
//!   --reviewer-key FILE --publisher ID --address ADDR --domain DOMAIN --provenance 0x<32 bytes>
//!   --note TEXT [--cast PATH]`
//!
//! Before anything is sent: the chain is not a local anvil (that rehearsal is
//! `scripts/bulk-demo.sh local`), and the book's `reviewer()` is the key's address.

use onyx_coordinator::cast_chain::CastChain;
use onyx_coordinator::keyfile::{key_address, read_key_file};
use onyx_level2::live::KeySigner;
use std::path::Path;

fn flag(args: &[String], name: &str) -> Result<String, String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .ok_or(format!("{name} is required"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run(args: &[String]) -> Result<(), String> {
    if args.get(1).map(String::as_str) != Some("register-key") {
        return Err("usage: book-reviewer register-key --rpc URL --book ADDR --reviewer-key FILE --publisher ID --address ADDR --domain DOMAIN --provenance 0x.. --note TEXT [--cast PATH]".into());
    }
    let secret = read_key_file(Path::new(&flag(args, "--reviewer-key")?))?;
    let reviewer = key_address(&secret).ok_or("--reviewer-key is not a valid key")?;
    let provenance = flag(args, "--provenance")?;
    let p = provenance.trim_start_matches("0x");
    if p.len() != 64 || !p.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("--provenance must be 32 bytes of hex".into());
    }
    let mut chain_args = vec![
        "--rpc".to_string(),
        flag(args, "--rpc")?,
        "--registry".into(),
        flag(args, "--book")?,
        "--coordinator".into(),
        format!("0x{}", hex(&reviewer)),
    ];
    if let Ok(c) = flag(args, "--cast") {
        chain_args.extend(["--cast".to_string(), c]);
    }
    let mut chain = CastChain::from_args(&chain_args)?.ok_or("--rpc is required")?;
    // KeySigner::new refuses a local anvil.
    chain
        .signers
        .push(std::sync::Arc::new(KeySigner::new(&chain, secret)?));
    let on_book = chain.call("reviewer()(address)", &[])?;
    if on_book.trim().to_ascii_lowercase() != format!("0x{}", hex(&reviewer)) {
        return Err(format!(
            "the book's reviewer is {}, not this key's 0x{}: nothing sent",
            on_book.trim(),
            hex(&reviewer)
        ));
    }
    let block = chain.send(
        &reviewer,
        "registerKey(string,address,string,bytes32,string)",
        &[
            flag(args, "--publisher")?,
            flag(args, "--address")?,
            flag(args, "--domain")?,
            format!("0x{p}"),
            flag(args, "--note")?,
        ],
    )?;
    println!(
        "REGISTERED {} {} in block {block}",
        flag(args, "--publisher")?,
        flag(args, "--address")?
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
