//! Benchmark `LabelSet::check_attestations` (L12-F5): N labels, each citing a
//! real attestation (BIP-322 P2WPKH full proof + EIP-191 entity signature).
//! Run: `cargo run --release -p onyx-source-btc --example bench-attest -- 20000`.
//! Hostile mode, each address a P2WSH 16-of-16 multisig (the most expensive
//! valid proof): `... -- 1000 ms16`. Worst case per byte (L14-F8): a
//! 1-of-16 multisig signed by the last key, so every key is tried:
//! `... -- 58000 ms1of16` (about the 64 MiB attestation-byte cap).
//! Keys are derived from fixed labels; test only. Floats are for timing output only.
#![allow(clippy::float_arithmetic)]

use bip322::LockParams;
use bitcoin::key::{CompressedPublicKey, PrivateKey};
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::{opcodes, Address, Network, ScriptBuf};
use onyx_core::{keccak256, Confidence, EntityKind, EvidenceRef, StaticRegistry};
use onyx_labels::attest::{eip191_digest, AttestContext, Attestation, Statement};
use onyx_labels::{Label, LabelSet, ATTESTATION_EVIDENCE};
use onyx_source_btc::{Bip322Proof, BtcAddressCanon};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::args().nth(1).map_or(Ok(20_000), |a| a.parse())?;
    let mode = std::env::args().nth(2).unwrap_or_default();
    let (ms16, one_of_16) = (mode == "ms16" || mode == "ms1of16", mode == "ms1of16");
    let secp = Secp256k1::new();
    let entity = SecretKey::from_slice(&keccak256(b"onyx test entity key 1").0)?;
    let entity_pk = entity.public_key(&secp).serialize_uncompressed();
    let mut entity_key = [0u8; 20];
    entity_key.copy_from_slice(&keccak256(&entity_pk[1..]).0[12..]);

    let mut reg = StaticRegistry::new();
    reg.set_domain("onyx.devnet")
        .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(Bip322Proof)))
        .and_then(|r| r.add_entity_key("entity:example", entity_key))?;

    let t = Instant::now();
    let mut atts = Vec::with_capacity(n);
    let mut labels = Vec::with_capacity(n);
    for i in 0..n {
        let sk = SecretKey::from_slice(&keccak256(format!("onyx bench key {i}").as_bytes()).0)?;
        let ms_keys: Vec<SecretKey> = if ms16 {
            (0..16)
                .map(|j| {
                    SecretKey::from_slice(&keccak256(format!("onyx bench ms {i} {j}").as_bytes()).0)
                })
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let witness_script = ms16.then(|| {
            let mut b = ScriptBuf::builder().push_int(if one_of_16 { 1 } else { 16 });
            for k in &ms_keys {
                b = b.push_key(&bitcoin::PublicKey::new(k.public_key(&secp)));
            }
            b.push_int(16)
                .push_opcode(opcodes::all::OP_CHECKMULTISIG)
                .into_script()
        });
        let addr = match &witness_script {
            Some(ws) => Address::p2wsh(ws, Network::Bitcoin).to_string(),
            None => Address::p2wpkh(&CompressedPublicKey(sk.public_key(&secp)), Network::Bitcoin)
                .to_string(),
        };
        let text = Statement {
            domain: "onyx.devnet".into(),
            entity: "entity:example".into(),
            address: format!("btc:{addr}"),
            valid_from: 921_000,
            expires: 930_000,
            nonce: [1; 16],
        }
        .render();
        let address_sig = if let Some(ws) = &witness_script {
            let signers = if one_of_16 {
                &ms_keys[15..]
            } else {
                &ms_keys[..]
            };
            let wifs: Vec<String> = signers
                .iter()
                .map(|k| PrivateKey::new(*k, Network::Bitcoin).to_wif())
                .collect();
            let hex = ws.to_hex_string();
            bip322::sign_full_encoded(&addr, &text, &wifs, Some(&hex), LockParams::default())?
        } else {
            let wif = PrivateKey::new(sk, Network::Bitcoin).to_wif();
            bip322::sign_full_encoded(&addr, &text, &[wif], None, LockParams::default())?
        };
        let (recid, rs) = secp
            .sign_ecdsa_recoverable(
                &Message::from_digest(eip191_digest(text.as_bytes()).0),
                &entity,
            )
            .serialize_compact();
        let mut entity_sig = [0u8; 65];
        entity_sig[..64].copy_from_slice(&rs);
        entity_sig[64] = 27 + u8::try_from(recid.to_i32())?;
        let a = Attestation {
            statement: text,
            address_sig,
            entity_key,
            entity_sig,
        };
        labels.push(Label {
            address: format!("btc:{addr}"),
            entity: "entity:example".into(),
            kind: EntityKind::Exchange,
            confidence: Confidence(Confidence::ONE),
            evidence: vec![EvidenceRef {
                kind: ATTESTATION_EVIDENCE.into(),
                content_hash: a.content_hash(),
            }],
        });
        atts.push(a);
    }
    labels.sort_by(|a, b| a.address.cmp(&b.address));
    let set = LabelSet {
        name: "onyx.bench".into(),
        version: 1,
        labels,
    };
    let build_s = t.elapsed().as_secs_f64();

    let ctx = AttestContext {
        ranges: &[("btc", 921_080, 921_082)],
    };
    let t = Instant::now();
    set.validate(&reg)?;
    let validate_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    set.check_attestations(&atts, &ctx, &reg)?;
    let check_s = t.elapsed().as_secs_f64();
    println!(
        "labels {n}: build {build_s:.2} s, validate {validate_s:.3} s, check_attestations {check_s:.3} s ({:.1} us per label)",
        check_s * 1e6 / n as f64
    );
    Ok(())
}
