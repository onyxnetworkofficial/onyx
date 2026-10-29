//! ROADMAP S2b verifier: what Onyx would run on a submitted presentation. Offline.
//!
//! Usage: `verify PRESENTATION --notary-key HEX [--now UNIX] [--max-age-secs N]`
//! Exit 0 only when every check in the library passes.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use onyx_tlsn_fiscaldata as x;

fn main() -> Result<()> {
    let mut path = None;
    let mut key = None;
    let mut now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut max_age = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--notary-key" => key = Some(hex::decode(args.next().context("--notary-key HEX")?)?),
            "--now" => now = args.next().context("--now UNIX")?.parse()?,
            "--max-age-secs" => max_age = Some(args.next().context("--max-age-secs N")?.parse()?),
            p if path.is_none() && !p.starts_with("--") => path = Some(p.to_string()),
            _ => {
                bail!("usage: verify PRESENTATION --notary-key HEX [--now UNIX] [--max-age-secs N]")
            }
        }
    }
    let path = path.context("PRESENTATION path missing")?;
    let key =
        key.context("--notary-key is required: the verifier must pin the notary it trusts")?;
    let bytes = std::fs::read(&path)?;
    let t0 = std::time::Instant::now();
    let c = x::verify_presentation(&bytes, &x::Policy::fiscaldata(key, now, max_age))?;
    let us = t0.elapsed().as_micros();

    println!("VERIFIED ({us} µs, {} bytes)", bytes.len());
    println!("server name:      {}", c.server_name);
    println!("tls:              {}", c.tls_version);
    println!(
        "session time:     {} (unix; age {} s)",
        c.connection_time,
        now.saturating_sub(c.connection_time)
    );
    println!(
        "notary key:       secp256k1 {} (trusted: pinned by the caller)",
        c.notary_key_hex
    );
    println!("request target:   {}", c.request_target);
    println!(
        "hidden header:    {} value, {} bytes at sent[{}..{}], all hidden (the verified transcript holds only zeros there)",
        c.redacted_header,
        c.redacted_range.len(),
        c.redacted_range.start,
        c.redacted_range.end
    );
    println!(
        "hidden response:  {} bytes (Set-Cookie values only)",
        c.hidden_response_bytes
    );
    println!("status:           {}", c.status);
    println!(
        "body:             {} bytes, keccak256 {}",
        c.body.len(),
        c.body_keccak256
    );
    println!(
        "value:            {} = {} (record date {})",
        x::JSON_POINTER,
        c.value,
        c.record_date
    );
    println!("--- request as revealed (hidden bytes shown as X) ---");
    println!("{}", c.request_shown.trim_end());
    Ok(())
}
