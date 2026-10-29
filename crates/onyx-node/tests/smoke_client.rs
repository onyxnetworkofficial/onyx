//! Smoke client for a running onyx-node (docs/ops/deployment.md): one paid
//! GET, signed with a throwaway test key. Ignored by default; run with
//! `ONYX_SMOKE_ADDR=127.0.0.1:port ONYX_SMOKE_PATH=/v1/proofs/... \
//! ONYX_SMOKE_KEY_FILE=payer.key cargo test -p onyx-node --test smoke_client
//! -- --ignored --nocapture` (`ONYX_SMOKE_NONCE` picks the EIP-3009 nonce
//! byte, `ONYX_SMOKE_VALID_FOR` the authorization's life in seconds, and
//! `ONYX_SMOKE_EXPECT` the HTTP status the run must get, 200 by default:
//! a cut connection or an empty answer fails it, OPS-L4-F6). The key file holds 64 hex digits of a fresh key (e.g. from `cast
//! wallet new`), funded with test USDC on the fork. Never use a well-known
//! key: on Base Sepolia some carry an EIP-7702 delegation, and USDC then
//! checks the signature as a contract wallet (ERC-1271) and refuses it
//! ("FiatTokenV2: invalid signature"; review 2's smoke failure, OPS-L2).
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
use onyx_api::x402::{decode_header, encode_header, exact_evm, PaymentPayload, PaymentRequired};
use std::io::{Read, Write};

fn get(addr: &str, path: &str, extra: &str) -> (String, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{extra}\r\n"
    )
    .unwrap();
    let mut b = String::new();
    let _ = s.read_to_string(&mut b);
    let (h, body) = b.split_once("\r\n\r\n").unwrap_or((&b, ""));
    (h.to_string(), body.to_string())
}

#[test]
#[ignore]
fn paid_get() {
    let addr = std::env::var("ONYX_SMOKE_ADDR").unwrap();
    let path = std::env::var("ONYX_SMOKE_PATH").unwrap();
    let nonce: u8 = std::env::var("ONYX_SMOKE_NONCE")
        .map(|v| v.parse().unwrap())
        .unwrap_or(1);
    let (h, b) = get(&addr, &path, "");
    let line = h
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("payment-required:"))
        .unwrap_or_else(|| panic!("no challenge: {h}\n{b}"));
    let pr: PaymentRequired = decode_header(line.split_once(':').unwrap().1.trim()).unwrap();
    let accepted = pr.accepts[0].clone();
    let key_file =
        std::env::var("ONYX_SMOKE_KEY_FILE").expect("ONYX_SMOKE_KEY_FILE (a fresh test key)");
    let secret: [u8; 32] = hex::decode(
        std::fs::read_to_string(key_file)
            .unwrap()
            .trim()
            .trim_start_matches("0x"),
    )
    .ok()
    .and_then(|b| b.try_into().ok())
    .expect("64 hex digits");
    let mut to = [0u8; 20];
    to.copy_from_slice(&hex::decode(&accepted.pay_to[2..]).unwrap());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let valid_for = std::env::var("ONYX_SMOKE_VALID_FOR")
        .map(|v| v.parse::<u64>().unwrap())
        .unwrap_or(accepted.max_timeout_seconds);
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&secret).unwrap(),
        to,
        value: accepted.amount.parse().unwrap(),
        // As an x402 client signs it (OPS-L3-F4 refuses a longer life,
        // OPS-L4-F1 a shorter one than the settle budget).
        valid_after: now.saturating_sub(600),
        valid_before: now + valid_for,
        nonce: [nonce; 32],
    };
    eprintln!(
        "payer 0x{} nonce 0x{} validBefore {}",
        hex::encode(auth.from),
        hex::encode(auth.nonce),
        auth.valid_before
    );
    let hdr = encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&secret, &auth, &accepted).unwrap(),
        accepted,
    });
    let t = std::time::Instant::now();
    let (h, b) = get(&addr, &path, &format!("payment-signature: {hdr}\r\n"));
    let status = h.lines().next().unwrap_or("");
    eprintln!(
        "paid GET after {:?}: {status}\n{}",
        t.elapsed(),
        &b[..b.len().min(300)]
    );
    let expect = std::env::var("ONYX_SMOKE_EXPECT").unwrap_or_else(|_| "200".into());
    assert!(
        status.split_whitespace().nth(1) == Some(expect.as_str()),
        "expected HTTP {expect}, got {status:?} (empty means the connection was cut)"
    );
}
