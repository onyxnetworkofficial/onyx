//! Keyed level-2 rounds (contributor loop CL4, ADR-0054): a keyed manifest source, the key
//! holders' seats, the round's keyed pool, the live coordinator and observer, and a loopback
//! fake keyed provider for the local demo (`scripts/level2-keyed-anvil.sh`).
//!
//! `cargo run -p onyx-level2 --example level2-keyed -- <mode> --manifest FILE [--repo DIR] ...`
//!
//! - `seat --key-file K [--operator OP] [--joined-at UNIX]`: the holder's signed seat (JSON on
//!   stdout): observer, source, operator, terms hash, time, signature. Nothing about the key.
//!   `onyx-contrib keyed join` runs this after the holder accepted the terms.
//! - `seats --seat FILE... --bond N --bonded-at UNIX --agreed N`: the coordinator's seats file
//!   (the seats with what it attests about each: bond, bond time, agreed public rounds).
//! - `pool --seats FILE --record-date D --window-start W`: the round's keyed snapshot as a pool
//!   file, or (exit 3) the level-4 statement when too few seats are drawable.
//! - `coordinator ...`: `onyx_level2::live::coordinator` (flags as `level2-manifest`, plus
//!   `--seats FILE`); with too few drawable seats it prints the level-4 statement and runs no
//!   round.
//! - `observer ... --api-key-stdin [--seats FILE] [--fixture HOST:PORT]`: one keyed observer,
//!   its own provider key on standard input (never a flag, file or environment variable).
//!   `--fixture` sends the fetch to a local fake provider (every `onyx_fetch` rule but the last
//!   hop); refused unless the chain is a local anvil (31337).
//! - `provider --answers FILE --port-file FILE`: the loopback fake keyed provider. `FILE` lists
//!   `{"key_keccak": "0x…", "value": "1234.5"}`: it keeps no key, only each key's Keccak-256,
//!   answers a known key's request with the manifest's row for the request's date and that
//!   key's value, refuses any other request (401), and logs counts only.
//! - `pin`: `metric=0x<template id>`.

use onyx_fetch::fixture::{FixedResolver, LoopbackDial, FIXTURE_PUBLIC_IP};
use onyx_level2::keyed::{
    one_holder_statement, parse_seats, pool_to_json, seats_to_json, sign_seat, snapshot_for,
    KeyedError, KeyedSeat, SeatEntry,
};
use onyx_level2::manifest::ManifestSource;
use onyx_level2::Source;
use std::path::PathBuf;
use std::sync::Arc;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or(format!("{name} is required"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("level2-keyed: {e}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32, String> {
    let mode = args.get(1).cloned().unwrap_or_default();
    let path = PathBuf::from(need(args, "--manifest")?);
    let repo = PathBuf::from(flag(args, "--repo").unwrap_or_else(|| ".".into()));
    let src = ManifestSource::from_file(&path, &repo).map_err(|e| e.to_string())?;
    let (source_id, terms) = src
        .keyed_seat_rules()
        .ok_or("not a keyed manifest (its descriptor's access is public)")?;
    match mode.as_str() {
        "seat" => {
            let secret =
                onyx_coordinator::keyfile::read_key_file(&PathBuf::from(need(args, "--key-file")?))?;
            let joined_at = match flag(args, "--joined-at") {
                Some(t) => t.parse().map_err(|_| "--joined-at UNIX")?,
                None => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_secs(),
            };
            let seat = sign_seat(
                &secret,
                &source_id,
                flag(args, "--operator").as_deref(),
                terms,
                joined_at,
            )
            .map_err(|e| e.to_string())?;
            println!("{}", seat.to_json());
            Ok(0)
        }
        "seats" => {
            let bond: u128 = need(args, "--bond")?.parse().map_err(|_| "--bond N")?;
            let bonded_at: u64 = need(args, "--bonded-at")?
                .parse()
                .map_err(|_| "--bonded-at UNIX")?;
            let agreed: u32 = need(args, "--agreed")?.parse().map_err(|_| "--agreed N")?;
            let mut entries = vec![];
            for f in flags(args, "--seat") {
                let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                let v: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))?;
                let seat = KeyedSeat::from_json(&v).map_err(|e| format!("{f}: {e}"))?;
                if !seat.verify() {
                    return Err(format!("{f}: the signature is not the observer's"));
                }
                entries.push(SeatEntry {
                    seat,
                    bond,
                    bonded_at,
                    agreed_rounds: agreed,
                });
            }
            let out = seats_to_json(&entries);
            parse_seats(&out.to_string()).map_err(|e| e.to_string())?;
            println!("{out}");
            Ok(0)
        }
        "pool" => {
            let seats = std::fs::read_to_string(need(args, "--seats")?)
                .map_err(|e| format!("--seats: {e}"))?;
            let spec = src.spec(
                &need(args, "--record-date")?,
                need(args, "--window-start")?
                    .parse()
                    .map_err(|_| "--window-start UNIX")?,
            )?;
            match snapshot_for(&src, &seats, &spec) {
                Ok(s) => {
                    for (a, why) in &s.left_out {
                        eprintln!("seat 0x{}: left out ({why})", hex::encode(a));
                    }
                    println!("{}", pool_to_json(&s.pool)?);
                    Ok(0)
                }
                Err(e @ KeyedError::TooFewSeats { have, .. }) => {
                    println!("{}", one_holder_statement(&src, have, &e.to_string()));
                    Ok(3)
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "coordinator" => onyx_level2::live::coordinator(&src, args).map(|()| 0),
        "observer" => {
            let transport = match flag(args, "--fixture") {
                None => None,
                Some(addr) => Some(fixture_transport(&src, args, &addr)?),
            };
            Ok(onyx_level2::live::observer_with(&src, args, transport))
        }
        "provider" => provider(&src, args),
        "pin" => {
            let t = src.template();
            println!("{}={}", t.metric.0, t.id());
            Ok(0)
        }
        _ => Err("usage: level2-keyed <seat|seats|pool|coordinator|observer|provider|pin> --manifest FILE [--repo DIR] ...".into()),
    }
}

/// The local demo's transport: every `onyx_fetch` rule, the last hop to the loopback fake
/// provider. Only on a local anvil (chain 31337).
fn fixture_transport(
    src: &ManifestSource,
    args: &[String],
    addr: &str,
) -> Result<onyx_fetch::HttpsFetch, String> {
    let mut a = args.to_vec();
    if flag(args, "--coordinator").is_none() {
        a.extend(["--coordinator".into(), format!("0x{}", "00".repeat(20))]);
    }
    let chain = onyx_coordinator::cast_chain::CastChain::from_args(&a)?
        .ok_or("--fixture needs --rpc (a local anvil)")?;
    if chain.chain_id()? != onyx_coordinator::cast_chain::ANVIL_CHAIN_ID {
        return Err("--fixture is for a local anvil (chain 31337) only".into());
    }
    let addr: std::net::SocketAddr = addr.parse().map_err(|_| "--fixture HOST:PORT")?;
    if !addr.ip().is_loopback() {
        return Err("--fixture is a loopback address".into());
    }
    Ok(onyx_fetch::HttpsFetch::with_transport(
        Arc::new(FixedResolver::new(vec![FIXTURE_PUBLIC_IP])),
        Arc::new(LoopbackDial::new(addr)),
        src.fetch_limits(),
    ))
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The loopback fake keyed provider (see the module docs).
fn provider(src: &ManifestSource, args: &[String]) -> Result<i32, String> {
    use std::io::{Read, Write};
    let answers_text =
        std::fs::read_to_string(need(args, "--answers")?).map_err(|e| format!("--answers: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&answers_text).map_err(|e| e.to_string())?;
    let mut answers: Vec<([u8; 32], String)> = vec![];
    for a in v.as_array().ok_or("--answers: a list")? {
        let h = a["key_keccak"].as_str().ok_or("key_keccak")?;
        let mut k = [0u8; 32];
        hex::decode_to_slice(h.trim_start_matches("0x"), &mut k).map_err(|e| e.to_string())?;
        answers.push((k, a["value"].as_str().ok_or("value")?.to_string()));
    }
    let name = match src.key_placement() {
        Some(onyx_fetch::KeyPlacement::Query { name }) => name,
        _ => return Err("the fake provider serves query keys".into()),
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::fs::write(need(args, "--port-file")?, format!("{port}\n")).map_err(|e| e.to_string())?;
    eprintln!(
        "fake keyed provider on 127.0.0.1:{port} ({} keys, by hash only)",
        answers.len()
    );
    let (mut served, mut refused) = (0u32, 0u32);
    for conn in listener.incoming() {
        let Ok(mut sock) = conn else { continue };
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let text = String::from_utf8_lossy(&head).to_string();
        let target = text.split(' ').nth(1).unwrap_or("").to_string();
        let query = target.split_once('?').map_or("", |(_, q)| q);
        let param = |n: &str| {
            query
                .split('&')
                .find_map(|p| p.strip_prefix(&format!("{n}=")))
                .and_then(percent_decode)
        };
        let date = param("date").and_then(|d| String::from_utf8(d).ok());
        let value = param(&name).and_then(|k| {
            let h = onyx_core::keccak256(&k).0;
            answers
                .iter()
                .find(|(x, _)| *x == h)
                .map(|(_, v)| v.clone())
        });
        let (status, body) = match (date, value) {
            (Some(d), Some(v)) if onyx_core::feed::day_number(&d).is_some() => {
                served += 1;
                (
                    "200 OK",
                    format!(r#"{{"data":[{{"date":"{d}","index":"{v}"}}],"meta":{{"count":1}}}}"#),
                )
            }
            _ => {
                refused += 1;
                ("401 Unauthorized", r#"{"error":"unknown key"}"#.to_string())
            }
        };
        let _ = sock.write_all(
            format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        eprintln!("fake keyed provider: {served} answered, {refused} refused");
    }
    Ok(0)
}
