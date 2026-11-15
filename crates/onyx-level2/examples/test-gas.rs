//! Send a little test ETH (gas) from one of the operator's test keys to another test key's
//! address on Base Sepolia (ROADMAP S26 slice 3: the `early-record` publisher key pays one
//! anchor a day; docs/ops/early-record.md). The key is read from a 0600 file and signs in
//! this process (`KeySigner`): it is never on a command line, in the environment or printed.
//!
//! `cargo run -p onyx-level2 --example test-gas -- send --rpc URL --from-key FILE --to ADDR
//!   --wei N [--yes] [--cast PATH]`
//!
//! Without `--yes` it only prints what it would send. It refuses any chain but Base Sepolia
//! (84532; the chain id checked is the one the signer signs with), more than 0.001 test ETH in
//! one send, a sender that would keep less than the amount it sends after the fee, a send to
//! itself, and the zero address or a precompile.

use onyx_coordinator::cast_chain::CastChain;
use onyx_coordinator::keyfile::{key_address, read_key_file};
use onyx_level2::live::{KeySigner, MAX_FEE_PER_GAS};
use std::path::Path;

/// Base Sepolia, the only chain this tool sends on.
const BASE_SEPOLIA: u64 = 84532;
/// 0.001 test ETH: about 600 daily anchors at today's gas price.
const MAX_WEI: u128 = 1_000_000_000_000_000;
/// More gas than a plain value send can use (21,000 with the signer's margin).
const SEND_GAS: u128 = 40_000;

fn flag(args: &[String], name: &str) -> Result<String, String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .ok_or(format!("{name} is required"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A 0x address of 20 bytes.
fn parse_address(s: &str) -> Result<[u8; 20], String> {
    let h = s.strip_prefix("0x").ok_or("--to must start with 0x")?;
    if h.len() != 40 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("--to must be 20 bytes of hex".into());
    }
    let mut a = [0u8; 20];
    for (i, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    // The zero address and the precompiles (0x..01 to 0x..ff) would burn it.
    if a[..19].iter().all(|&b| b == 0) {
        return Err("--to is the zero address or a precompile: nothing would receive it".into());
    }
    Ok(a)
}

/// The amount: a whole number of wei, more than 0 and at most [`MAX_WEI`].
fn parse_wei(s: &str) -> Result<u128, String> {
    let w: u128 = s
        .parse()
        .map_err(|_| "--wei must be a whole number".to_string())?;
    if w == 0 || w > MAX_WEI {
        return Err(format!("--wei must be 1..={MAX_WEI} (0.001 test ETH)"));
    }
    Ok(w)
}

/// The refusals that need the chain's answers, kept apart so tests can check them.
fn check_send(
    chain_id: u64,
    from: &[u8; 20],
    to: &[u8; 20],
    wei: u128,
    balance: u128,
) -> Result<(), String> {
    if chain_id != BASE_SEPOLIA {
        return Err(format!(
            "the node is chain {chain_id}, not Base Sepolia ({BASE_SEPOLIA}): nothing sent"
        ));
    }
    if from == to {
        return Err("--to is the sender's own address: nothing sent".into());
    }
    // The send's own fee, at the signer's cap, comes off the sender too.
    let needed = wei
        .saturating_mul(2)
        .saturating_add(SEND_GAS.saturating_mul(MAX_FEE_PER_GAS));
    if balance < needed {
        return Err(format!(
            "the sender holds {balance} wei and would keep less than the {wei} wei it sends (with the fee it needs {needed}): nothing sent"
        ));
    }
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    if args.get(1).map(String::as_str) != Some("send") {
        return Err("usage: test-gas send --rpc URL --from-key FILE --to ADDR --wei N [--yes] [--cast PATH]".into());
    }
    let to = parse_address(&flag(args, "--to")?)?;
    let wei = parse_wei(&flag(args, "--wei")?)?;
    let secret = read_key_file(Path::new(&flag(args, "--from-key")?))?;
    let from = key_address(&secret).ok_or("--from-key is not a valid key")?;
    let from_hex = format!("0x{}", hex(&from));
    let mut chain_args = vec![
        "--rpc".to_string(),
        flag(args, "--rpc")?,
        "--registry".into(),
        from_hex.clone(),
        "--coordinator".into(),
        from_hex.clone(),
    ];
    if let Ok(c) = flag(args, "--cast") {
        chain_args.extend(["--cast".to_string(), c]);
    }
    let chain = CastChain::from_args(&chain_args)?.ok_or("--rpc is required")?;
    // The signer reads the chain id once and signs with that one, so that is the id
    // checked, not another answer of the node. KeySigner::new refuses a local anvil.
    // Making it sends nothing.
    let signer = KeySigner::new(&chain, secret)?;
    let balance: u128 = chain
        .run(&["balance".into(), from_hex.clone()])?
        .trim()
        .parse()
        .map_err(|e| format!("balance: {e}"))?;
    check_send(signer.chain_id(), &from, &to, wei, balance)?;
    let to_hex = format!("0x{}", hex(&to));
    if !args.iter().any(|a| a == "--yes") {
        println!("DRY RUN: would send {wei} wei from {from_hex} (holds {balance}) to {to_hex}; add --yes to send");
        return Ok(());
    }
    let block = signer.send_value(&to, wei)?;
    println!("SENT {wei} wei from {from_hex} to {to_hex} in block {block}");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_and_addresses_are_checked() {
        assert_eq!(parse_wei("1"), Ok(1));
        assert_eq!(parse_wei(&MAX_WEI.to_string()), Ok(MAX_WEI));
        assert!(parse_wei("0").is_err());
        assert!(parse_wei(&(MAX_WEI + 1).to_string()).is_err());
        assert!(parse_wei("1e15").is_err());
        assert!(parse_address("0x0000000000000000000000000000000000000100").is_ok());
        assert!(parse_address("0x1000000000000000000000000000000000000000").is_ok());
        assert!(parse_address("0000000000000000000000000000000000000100").is_err());
        assert!(parse_address("0x00").is_err());
        assert!(parse_address("0xzz00000000000000000000000000000000000100").is_err());
        assert!(
            parse_address("0x0000000000000000000000000000000000000000").is_err(),
            "zero"
        );
        assert!(
            parse_address("0x0000000000000000000000000000000000000001").is_err(),
            "precompile"
        );
        assert!(
            parse_address("0x00000000000000000000000000000000000000ff").is_err(),
            "precompile"
        );
    }

    #[test]
    fn only_base_sepolia_and_never_the_last_of_the_balance() {
        let (a, b) = ([1u8; 20], [2u8; 20]);
        let fee = SEND_GAS * MAX_FEE_PER_GAS;
        assert!(check_send(BASE_SEPOLIA, &a, &b, 10, 20 + fee).is_ok());
        assert!(
            check_send(BASE_SEPOLIA, &a, &b, 10, 19 + fee).is_err(),
            "would keep less than it sends"
        );
        assert!(
            check_send(BASE_SEPOLIA, &a, &b, 10, 20).is_err(),
            "the fee counts"
        );
        assert!(
            check_send(BASE_SEPOLIA, &a, &a, 10, 100 + fee).is_err(),
            "to itself"
        );
        assert!(
            check_send(8453, &a, &b, 10, 100 + fee).is_err(),
            "Base mainnet"
        );
        assert!(
            check_send(1, &a, &b, 10, 100 + fee).is_err(),
            "Ethereum mainnet"
        );
        assert!(
            check_send(31337, &a, &b, 10, 100 + fee).is_err(),
            "a local anvil"
        );
    }
}
